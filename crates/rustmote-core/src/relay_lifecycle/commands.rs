//! Pure command-string builders with argument allowlists.
//!
//! Every remote command issued by [`super::RelayLifecycle`] is constructed
//! here. Each builder validates its arguments against a strict allowlist
//! **before** string concatenation, so callers cannot accidentally
//! interpolate shell-meta characters into a `channel.exec()` call (spec
//! §5.1.6, §6.4).
//!
//! Path strings are restricted to `[A-Za-z0-9_./-]` and rejected if they
//! contain `..` components. Repo, tag, and digest strings follow Docker
//! Hub's published grammar subset. Base64 payloads are the only wildcard
//! input — they are ASCII-safe by construction and pass through `printf`
//! + `base64 -d`.

use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::error::RustmoteError;

// -----------------------------------------------------------------------------
// Validators
// -----------------------------------------------------------------------------

/// Validate an absolute remote path intended as an argument to a remote
/// shell command. Returns the validated string on success.
///
/// # Errors
/// Returns [`RustmoteError::InvalidRelayPath`] for relative paths,
/// disallowed characters, or any `..` component.
pub fn validate_remote_path(path: &Path) -> crate::Result<&str> {
    let s = path
        .to_str()
        .ok_or_else(|| RustmoteError::InvalidRelayPath(path.display().to_string()))?;
    if !s.starts_with('/') {
        return Err(RustmoteError::InvalidRelayPath(s.to_owned()));
    }
    for c in s.chars() {
        if !(c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')) {
            return Err(RustmoteError::InvalidRelayPath(s.to_owned()));
        }
    }
    if s.split('/').any(|c| c == "..") {
        return Err(RustmoteError::InvalidRelayPath(s.to_owned()));
    }
    Ok(s)
}

/// Validate a Docker Hub repository name (e.g. `rustdesk/rustdesk-server`).
///
/// # Errors
/// Returns [`RustmoteError::InvalidImageRef`] if the input contains
/// characters outside `[a-z0-9._/-]` or starts/ends with a separator.
pub fn validate_repo(repo: &str) -> crate::Result<&str> {
    if repo.is_empty() || repo.len() > 255 {
        return Err(RustmoteError::InvalidImageRef(format!("repo={repo}")));
    }
    for c in repo.chars() {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-' | '/')) {
            return Err(RustmoteError::InvalidImageRef(format!("repo={repo}")));
        }
    }
    if repo.starts_with(['/', '.', '-', '_']) || repo.ends_with(['/', '.', '-', '_']) {
        return Err(RustmoteError::InvalidImageRef(format!("repo={repo}")));
    }
    // Reject any `..` component — matters for repo strings that end up
    // in URLs even though `.` is otherwise legal within a component.
    if repo.split('/').any(|c| c == "..") {
        return Err(RustmoteError::InvalidImageRef(format!("repo={repo}")));
    }
    Ok(repo)
}

/// Validate a Docker image tag.
///
/// # Errors
/// Returns [`RustmoteError::InvalidImageRef`] for empty / overlong tags
/// or disallowed characters.
pub fn validate_tag(tag: &str) -> crate::Result<&str> {
    if tag.is_empty() || tag.len() > 128 {
        return Err(RustmoteError::InvalidImageRef(format!("tag={tag}")));
    }
    if tag.starts_with(['.', '-']) {
        return Err(RustmoteError::InvalidImageRef(format!("tag={tag}")));
    }
    for c in tag.chars() {
        if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
            return Err(RustmoteError::InvalidImageRef(format!("tag={tag}")));
        }
    }
    Ok(tag)
}

/// Validate a `sha256:<hex>` digest.
///
/// # Errors
/// Returns [`RustmoteError::InvalidImageRef`] for unknown algorithms or
/// non-hex payloads.
pub fn validate_digest(digest: &str) -> crate::Result<&str> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(RustmoteError::InvalidImageRef(format!("digest={digest}")));
    };
    if hex.len() != 64
        || !hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(RustmoteError::InvalidImageRef(format!("digest={digest}")));
    }
    Ok(digest)
}

/// Validate an ISO-8601 basic-ish timestamp used as a backup directory
/// suffix (e.g. `pre-update-2026-04-18T12-34-56Z`). We replace `:` with
/// `-` for filesystem compatibility; caller must already have done so.
///
/// # Errors
/// Returns [`RustmoteError::InvalidRelayPath`] for disallowed characters.
pub fn validate_backup_suffix(suffix: &str) -> crate::Result<&str> {
    if suffix.is_empty() || suffix.len() > 64 {
        return Err(RustmoteError::InvalidRelayPath(format!(
            "backup-suffix={suffix}"
        )));
    }
    for c in suffix.chars() {
        if !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            return Err(RustmoteError::InvalidRelayPath(format!(
                "backup-suffix={suffix}"
            )));
        }
    }
    Ok(suffix)
}

// -----------------------------------------------------------------------------
// Command builders
// -----------------------------------------------------------------------------

/// `cat /etc/os-release` — parsed by caller to decide Debian vs Arch etc.
#[must_use]
pub fn read_os_release() -> String {
    "cat /etc/os-release".to_string()
}

/// `command -v docker` — zero exit means docker binary is on PATH.
#[must_use]
pub fn check_docker_present() -> String {
    "command -v docker".to_string()
}

/// `docker compose version` — zero exit means the v2 plugin is installed.
#[must_use]
pub fn check_docker_compose_v2() -> String {
    "docker compose version".to_string()
}

/// `docker-compose --version` — zero exit on v1-only hosts. We check
/// this so we can abort with [`RustmoteError::DockerComposeV1Detected`]
/// rather than silently using the broken legacy plugin.
#[must_use]
pub fn check_docker_compose_v1() -> String {
    "docker-compose --version".to_string()
}

/// `test -d {path}` — zero exit means the directory exists.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn test_dir_exists(path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(path)?;
    Ok(format!("test -d {p}"))
}

/// `test -f {path}` — zero exit means the file exists.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn test_file_exists(path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(path)?;
    Ok(format!("test -f {p}"))
}

/// `mkdir -p {path}` — idempotent.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn mkdir_p(path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(path)?;
    Ok(format!("mkdir -p {p}"))
}

/// `cat {path}` — read a file.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn cat_file(path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(path)?;
    Ok(format!("cat {p}"))
}

/// `cp -a {src} {dst}` — used for backup snapshots.
///
/// # Errors
/// Propagates [`validate_remote_path`] on either argument.
pub fn copy_file(src: &Path, dst: &Path) -> crate::Result<String> {
    let s = validate_remote_path(src)?;
    let d = validate_remote_path(dst)?;
    Ok(format!("cp -a {s} {d}"))
}

/// Write `content` to `path` via `printf` + `base64 -d`, then `mv` to
/// the final location with mode 0644. Using base64 sidesteps all shell
/// escaping concerns — the payload is ASCII-safe by construction.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn write_file(path: &Path, content: &[u8]) -> crate::Result<String> {
    let p = validate_remote_path(path)?;
    let encoded = B64.encode(content);
    Ok(format!(
        "printf %s {encoded} | base64 -d > {p}.rustmote-tmp \
         && mv {p}.rustmote-tmp {p} && chmod 0644 {p}"
    ))
}

/// `cd {install_path} && docker compose up -d`.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn compose_up(install_path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(install_path)?;
    Ok(format!("cd {p} && docker compose up -d"))
}

/// `cd {install_path} && docker compose pull`.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn compose_pull(install_path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(install_path)?;
    Ok(format!("cd {p} && docker compose pull"))
}

/// `cd {install_path} && docker compose config` — used to snapshot the
/// effective compose config as part of a pre-update backup.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn compose_config(install_path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(install_path)?;
    Ok(format!("cd {p} && docker compose config"))
}

/// `cd {install_path} && docker compose ps --format json`.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn compose_ps_json(install_path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(install_path)?;
    Ok(format!("cd {p} && docker compose ps --format json"))
}

/// `cd {install_path} && docker compose logs --no-color --tail 200`.
///
/// # Errors
/// Propagates [`validate_remote_path`].
pub fn compose_logs_tail(install_path: &Path) -> crate::Result<String> {
    let p = validate_remote_path(install_path)?;
    Ok(format!(
        "cd {p} && docker compose logs --no-color --tail 200"
    ))
}

/// Probe a TCP port on the remote's loopback interface via bash's
/// `/dev/tcp` built-in. Exits 0 on a successful TCP connect. Works on
/// default bash (Debian, Ubuntu, Arch ship bash).
///
/// We wrap with `bash -c` explicitly because the default remote shell
/// on some systems is dash, which does not implement `/dev/tcp`.
#[must_use]
pub fn tcp_probe(port: u16) -> String {
    // `exec 3<>/dev/tcp/...` fails non-zero if the connect fails. We
    // redirect stderr so health-check spam doesn't pollute the SSH
    // channel.
    format!("bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}' 2>/dev/null")
}

/// Build the `image:` value for a compose file: `repo:tag@sha256:...`.
///
/// # Errors
/// Propagates [`validate_repo`] / [`validate_tag`] / [`validate_digest`].
pub fn pinned_image_ref(repo: &str, tag: &str, digest: &str) -> crate::Result<String> {
    let r = validate_repo(repo)?;
    let t = validate_tag(tag)?;
    let d = validate_digest(digest)?;
    Ok(format!("{r}:{t}@{d}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn validate_remote_path_accepts_simple_absolute() {
        assert_eq!(
            validate_remote_path(&PathBuf::from("/opt/rustmote-relay")).unwrap(),
            "/opt/rustmote-relay"
        );
    }

    #[test]
    fn validate_remote_path_rejects_relative_and_parent_components() {
        assert!(validate_remote_path(&PathBuf::from("opt/rustmote")).is_err());
        assert!(validate_remote_path(&PathBuf::from("/opt/../etc")).is_err());
    }

    #[test]
    fn validate_remote_path_rejects_shell_meta() {
        for bad in [
            "/opt/$HOME",
            "/opt/foo bar",
            "/opt/`whoami`",
            "/opt/;rm",
            "/opt/foo\n",
        ] {
            assert!(
                validate_remote_path(&PathBuf::from(bad)).is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn validate_repo_accepts_canonical_rustdesk() {
        assert_eq!(
            validate_repo("rustdesk/rustdesk-server").unwrap(),
            "rustdesk/rustdesk-server"
        );
    }

    #[test]
    fn validate_repo_rejects_uppercase_or_meta() {
        for bad in [
            "RustDesk/rustdesk-server",
            "rustdesk/rustdesk;server",
            "rustdesk/../etc",
        ] {
            assert!(validate_repo(bad).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn validate_tag_accepts_common_semver() {
        for ok in ["1.1.11", "1.2.3-rc1", "latest"] {
            assert_eq!(validate_tag(ok).unwrap(), ok);
        }
    }

    #[test]
    fn validate_tag_rejects_leading_dot_or_dash_or_meta() {
        for bad in [".hidden", "-dash", "tag with space", "tag;evil"] {
            assert!(validate_tag(bad).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn validate_digest_accepts_sha256_hex() {
        let d = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(validate_digest(d).unwrap(), d);
    }

    #[test]
    fn validate_digest_rejects_uppercase_short_or_wrong_alg() {
        for bad in [
            "sha512:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256:ABC",
            "sha256:0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(validate_digest(bad).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn write_file_emits_base64_and_moves_in_place() {
        let cmd = write_file(
            &PathBuf::from("/opt/rustmote-relay/docker-compose.yml"),
            b"hello\n",
        )
        .unwrap();
        assert!(cmd.contains("| base64 -d >"));
        assert!(cmd.contains("/opt/rustmote-relay/docker-compose.yml.rustmote-tmp"));
        assert!(cmd.contains("mv /opt/rustmote-relay/docker-compose.yml.rustmote-tmp"));
        assert!(cmd.contains("chmod 0644 /opt/rustmote-relay/docker-compose.yml"));
        // "hello\n" base64 is "aGVsbG8K"
        assert!(cmd.contains("aGVsbG8K"));
    }

    #[test]
    fn compose_up_and_pull_cd_into_validated_path() {
        let p = PathBuf::from("/opt/rustmote-relay");
        assert_eq!(
            compose_up(&p).unwrap(),
            "cd /opt/rustmote-relay && docker compose up -d"
        );
        assert_eq!(
            compose_pull(&p).unwrap(),
            "cd /opt/rustmote-relay && docker compose pull"
        );
    }

    #[test]
    fn tcp_probe_uses_bash_dev_tcp_with_port() {
        assert_eq!(
            tcp_probe(21116),
            "bash -c 'exec 3<>/dev/tcp/127.0.0.1/21116' 2>/dev/null"
        );
    }

    #[test]
    fn pinned_image_ref_round_trips() {
        let r = pinned_image_ref(
            "rustdesk/rustdesk-server",
            "1.1.11",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        assert_eq!(
            r,
            "rustdesk/rustdesk-server:1.1.11@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
    }
}
