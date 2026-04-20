//! RustDesk viewer binary detection and invocation (spec §3.5).
//!
//! This is the **one sanctioned shell-out path** in `rustmote-core`. Every
//! other remote command is issued through the `session` module's SSH
//! channel API; only the viewer needs a local `Command::spawn`. To keep
//! that narrow window safe:
//!
//! - The target ID is wrapped in [`TargetId`], which refuses anything but
//!   `^[0-9]{9,10}$` per spec §3.5.
//! - Argv is constructed with `Command::arg`, never through `sh -c` or
//!   string formatting — there is no shell in the invocation chain, so
//!   shell metacharacters are inert.
//! - Binary paths come from a fixed per-OS allow-list plus `$PATH`
//!   lookup on Unix. No caller-supplied path is executed unless it's
//!   been passed through [`Viewer::detect_with_override`] and confirmed
//!   to exist as a regular file.
//!
//! ## Per-OS detection (§3.5)
//!
//! - **Linux**: `$PATH` lookup for `rustdesk` → `/usr/bin/rustdesk` →
//!   `/opt/rustdesk/rustdesk` → `flatpak run com.rustdesk.RustDesk`
//!   (detected by probing `flatpak info com.rustdesk.RustDesk`).
//! - **Windows**: `%PROGRAMFILES%\RustDesk\rustdesk.exe` and the x86
//!   `%PROGRAMFILES(X86)%` variant. Registry-query fallback is
//!   mentioned in the spec but deferred — flag-for-owner if neither
//!   path exists rather than adding a `winreg` dependency for v0.1.
//! - **macOS**: `/Applications/RustDesk.app/Contents/MacOS/rustdesk`.
//!
//! ## Argv shape
//!
//! RustDesk CLI flags drift between 1.2.x and 1.3.x (spec §3.5
//! explicitly flags this). v0.1 targets 1.3.x and uses `--connect <id>`.
//! When the session module's port forward is in effect, RustDesk picks
//! up its configured rendezvous server — redirecting that server to
//! `127.0.0.1:<forwarded>` is a follow-up (open question, TASK-017).
//!
//! The supported RustDesk version range is documented in `README.md`
//! per spec §3.5.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::error::RustmoteError;

// -----------------------------------------------------------------------------
// TargetId — validated newtype
// -----------------------------------------------------------------------------

/// A RustDesk target ID. Constructed only via [`TargetId::new`], which
/// enforces the spec §3.5 regex `^[0-9]{9,10}$`. Wrapping the string
/// rather than accepting `&str` at the invocation site removes the
/// possibility of forwarding unvalidated user input into argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetId(String);

impl TargetId {
    /// Parse `raw` as a 9- or 10-digit RustDesk ID.
    ///
    /// # Errors
    /// [`RustmoteError::InvalidTargetId`] when `raw` is not 9–10 ASCII digits.
    pub fn new(raw: &str) -> crate::Result<Self> {
        let len = raw.len();
        if (9..=10).contains(&len) && raw.bytes().all(|b| b.is_ascii_digit()) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(RustmoteError::InvalidTargetId(raw.to_owned()))
        }
    }

    /// The validated ID as a `&str`. Safe to pass to `Command::arg`
    /// because the constructor has already excluded anything but digits.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TargetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// -----------------------------------------------------------------------------
// Viewer kinds
// -----------------------------------------------------------------------------

/// How the detected RustDesk viewer is launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerKind {
    /// A native binary at the given path.
    Native(PathBuf),
    /// The Flatpak app `com.rustdesk.RustDesk`, launched via `flatpak run`.
    Flatpak,
}

/// A detected RustDesk viewer plus its invocation style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    kind: ViewerKind,
}

impl Viewer {
    /// Detect a RustDesk viewer using spec §3.5's per-OS search order.
    ///
    /// # Errors
    /// [`RustmoteError::ViewerNotFound`] when no candidate path resolves
    /// to an existing file and Flatpak is unavailable or not installed.
    pub fn detect() -> crate::Result<Self> {
        Self::detect_with_override(None)
    }

    /// Same as [`Self::detect`] but with an optional user-supplied path
    /// override from `viewer_path` in the config. The override is still
    /// validated as a regular existing file — the caller cannot cause
    /// us to exec an arbitrary string.
    ///
    /// # Errors
    /// [`RustmoteError::ViewerNotFound`] on exhaustion of all candidates.
    pub fn detect_with_override(override_path: Option<&Path>) -> crate::Result<Self> {
        if let Some(p) = override_path {
            if p.is_file() {
                return Ok(Self {
                    kind: ViewerKind::Native(p.to_path_buf()),
                });
            }
            // A configured override that doesn't exist is a louder
            // failure than ViewerNotFound — fall through to the
            // standard search so the user at least gets a working
            // viewer if one is present elsewhere.
            tracing::warn!(
                path = %p.display(),
                "configured viewer_path does not exist; falling back to spec §3.5 search order",
            );
        }

        for candidate in default_candidates() {
            if candidate.is_file() {
                return Ok(Self {
                    kind: ViewerKind::Native(candidate),
                });
            }
        }

        #[cfg(target_os = "linux")]
        if flatpak_available() {
            return Ok(Self {
                kind: ViewerKind::Flatpak,
            });
        }

        Err(RustmoteError::ViewerNotFound)
    }

    /// The detected viewer kind (native path or Flatpak).
    #[must_use]
    pub fn kind(&self) -> &ViewerKind {
        &self.kind
    }

    /// Build the launch [`Command`] without spawning it. Useful for
    /// tests and for layers that want to set environment or stdio
    /// before `spawn`. Arguments are added via `Command::arg`, so
    /// shell metacharacters in `target` are impossible (and in any
    /// case the [`TargetId`] constructor has already excluded them).
    #[must_use]
    pub fn command(&self, target: &TargetId) -> Command {
        match &self.kind {
            ViewerKind::Native(path) => {
                let mut cmd = Command::new(path);
                cmd.arg("--connect").arg(target.as_str());
                cmd
            }
            ViewerKind::Flatpak => {
                let mut cmd = Command::new("flatpak");
                cmd.arg("run")
                    .arg("com.rustdesk.RustDesk")
                    .arg("--connect")
                    .arg(target.as_str());
                cmd
            }
        }
    }

    /// Spawn the viewer targeting `target`.
    ///
    /// # Errors
    /// [`RustmoteError::Io`] when the underlying `Command::spawn` fails
    /// (for example, if the binary was deleted between detection and
    /// launch).
    pub fn launch(&self, target: &TargetId) -> crate::Result<Child> {
        Ok(self.command(target).spawn()?)
    }
}

// -----------------------------------------------------------------------------
// Per-OS candidate lists
// -----------------------------------------------------------------------------

fn default_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let mut v = Vec::new();
        if let Some(p) = which_on_path("rustdesk") {
            v.push(p);
        }
        v.push(PathBuf::from("/usr/bin/rustdesk"));
        v.push(PathBuf::from("/opt/rustdesk/rustdesk"));
        v
    }
    #[cfg(target_os = "windows")]
    {
        let mut v = Vec::new();
        for env_key in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(env_key) {
                let mut p = PathBuf::from(root);
                p.push("RustDesk");
                p.push("rustdesk.exe");
                v.push(p);
            }
        }
        v
    }
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from(
            "/Applications/RustDesk.app/Contents/MacOS/rustdesk",
        )]
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// `$PATH` lookup for `name` — avoids shelling out to `which` and
/// keeps the one-shell-out policy (§6.4) honest. Returns the first
/// `PATH` entry where `{entry}/{name}` exists as a regular file.
#[cfg(target_os = "linux")]
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path_var: OsString = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn flatpak_available() -> bool {
    let Some(flatpak) = which_on_path("flatpak") else {
        return false;
    };
    // `flatpak info <app>` exits 0 when the app is installed, non-zero
    // otherwise. Not `info --show-ref` or similar — we don't care
    // about parsing output, just the exit status.
    Command::new(flatpak)
        .args(["info", "com.rustdesk.RustDesk"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_id_accepts_nine_digits() {
        let id = TargetId::new("123456789").unwrap();
        assert_eq!(id.as_str(), "123456789");
    }

    #[test]
    fn target_id_accepts_ten_digits() {
        TargetId::new("1234567890").unwrap();
    }

    #[test]
    fn target_id_rejects_eight_digits() {
        assert!(matches!(
            TargetId::new("12345678"),
            Err(RustmoteError::InvalidTargetId(_))
        ));
    }

    #[test]
    fn target_id_rejects_eleven_digits() {
        assert!(matches!(
            TargetId::new("12345678901"),
            Err(RustmoteError::InvalidTargetId(_))
        ));
    }

    #[test]
    fn target_id_rejects_empty() {
        assert!(matches!(
            TargetId::new(""),
            Err(RustmoteError::InvalidTargetId(_))
        ));
    }

    #[test]
    fn target_id_rejects_non_digits() {
        // Shell metacharacters, whitespace, letters — all must be refused
        // so they can never reach the Command builder.
        for bad in [
            "12345678a",
            "12345678 ",
            "1234-56789",
            "123;456789",
            "\u{1F60A}",
        ] {
            assert!(
                matches!(TargetId::new(bad), Err(RustmoteError::InvalidTargetId(_))),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn command_builds_for_native_viewer() {
        let viewer = Viewer {
            kind: ViewerKind::Native(PathBuf::from("/opt/rustdesk/rustdesk")),
        };
        let id = TargetId::new("123456789").unwrap();
        let cmd = viewer.command(&id);
        let program = cmd.get_program();
        assert_eq!(program, "/opt/rustdesk/rustdesk");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["--connect", "123456789"]);
    }

    #[test]
    fn command_builds_for_flatpak_viewer() {
        let viewer = Viewer {
            kind: ViewerKind::Flatpak,
        };
        let id = TargetId::new("9876543210").unwrap();
        let cmd = viewer.command(&id);
        assert_eq!(cmd.get_program(), "flatpak");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(
            args,
            ["run", "com.rustdesk.RustDesk", "--connect", "9876543210"]
        );
    }

    #[test]
    fn override_missing_file_falls_through() {
        let missing = std::path::Path::new("/tmp/rustmote-viewer-does-not-exist-9f8e7d6c5b4a");
        // We can't assert the result's Ok branch without a real
        // rustdesk install, but we can verify no panic and that a
        // non-existent override doesn't short-circuit with a
        // non-existent-file Native(path).
        let result = Viewer::detect_with_override(Some(missing));
        if let Ok(v) = result {
            assert_ne!(v.kind, ViewerKind::Native(missing.to_path_buf()));
        }
    }
}
