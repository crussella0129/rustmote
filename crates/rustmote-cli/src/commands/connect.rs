//! `rustmote connect` (spec §4.1, Phase 9 / TASK-009).
//!
//! The payoff command: wire `rustmote-core::session` to
//! `rustmote-core::viewer`. End-to-end flow:
//!
//! 1. Resolve `<target>` → a registered [`Target`] by id or label, or a
//!    standalone RustDesk ID validated via [`TargetId`] (spec §3.5).
//! 2. Resolve the relay to tunnel through: `--via` > target's
//!    `via_server` > `general.default_server`.
//! 3. Resolve the SSH user: `--user` override > server's configured
//!    `ssh_user`.
//! 4. Decide whether to pre-fetch a password for password-auth fallback.
//!    SSH keys are preferred (spec §3.4: "Prefer key-based auth.
//!    Implement password auth as fallback only"), so when
//!    `~/.ssh/id_ed25519` or `~/.ssh/id_rsa` exists we skip the prompt
//!    entirely and let russh try keys — prevents a needless password
//!    prompt on every connect.
//! 5. [`Session::open`] opens the SSH connection, performs mandatory
//!    host-key TOFU against [`KnownHosts`] (spec §6.7), and stands up
//!    the local → relay_port forward.
//! 6. On a newly-pinned host key, persist the store.
//! 7. Stamp `last_used` on the `RemoteServer` and save config.
//! 8. Detect and launch the RustDesk viewer with `--connect <target_id>`
//!    (the one sanctioned shell-out per spec §3.5). Wait for the viewer
//!    to exit before tearing down the SSH session.
//!
//! ## What's deferred to TASK-017 (spec §13 open questions)
//!
//! - Redirecting RustDesk's configured rendezvous server to
//!   `127.0.0.1:<forwarded-port>` — currently the port forward is
//!   established but RustDesk uses its own configured server; see
//!   `viewer.rs` module docs.
//! - Interactive passphrase prompt for encrypted SSH keys (spec §3.4).
//!   Today an encrypted key fails the load step and falls through to
//!   password auth.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use rustmote_core::config::Config;
use rustmote_core::credentials::CredentialStore;
use rustmote_core::registry::RemoteServer;
use rustmote_core::session::{AuthMaterial, KnownHosts, Session, TofuPolicy};
use rustmote_core::target::Target;
use rustmote_core::viewer::{TargetId, Viewer};
use rustmote_core::RustmoteError;

#[derive(Debug, Args)]
pub struct ConnectArgs {
    /// Target RustDesk ID (spec §3.5 regex) or registered label.
    pub target: String,
    /// Override the configured default server.
    #[arg(long)]
    pub via: Option<String>,
    /// Override SSH user for the underlying tunnel.
    #[arg(long)]
    pub user: Option<String>,
}

pub async fn run(args: ConnectArgs) -> Result<()> {
    let mut cfg = Config::load().context("loading config")?;

    let (target_id, matched_target) = resolve_target(&cfg, &args.target)?;
    let via_name = resolve_via_name(args.via.as_deref(), matched_target.as_ref(), &cfg)?;

    // Clone the registered server into a mutable binding so `--user`
    // can override `ssh_user` without mutating the registry itself.
    let mut effective_server = cfg
        .get_server(&via_name)
        .with_context(|| format!("resolving relay server '{via_name}'"))?
        .clone();
    if let Some(u) = &args.user {
        effective_server.ssh_user = u.clone();
    }

    let password = resolve_password(&cfg, &effective_server).await?;
    let auth = AuthMaterial {
        extra_key_paths: Vec::new(),
        key_passphrase: None,
        password,
    };

    let known_hosts = std::sync::Arc::new(std::sync::Mutex::new(
        KnownHosts::load().context("loading known_hosts.toml")?,
    ));

    tracing::info!(
        server = %effective_server.name,
        user = %effective_server.ssh_user,
        host = %effective_server.host,
        relay_port = effective_server.relay_port,
        target = %target_id,
        "opening SSH tunnel to relay"
    );

    let (session, newly_pinned) = Session::open(
        &effective_server,
        auth,
        std::sync::Arc::clone(&known_hosts),
        TofuPolicy::TrustOnFirstUse,
    )
    .await
    .context("opening SSH session to relay")?;

    if newly_pinned {
        known_hosts
            .lock()
            .expect("known_hosts mutex poisoned")
            .save()
            .context("persisting newly-pinned host key to known_hosts.toml")?;
        println!(
            "pinned new host key for {}:{} (TOFU first use)",
            effective_server.host, effective_server.ssh_port
        );
    }

    // Stamp last_used on the registered server (use the registry name,
    // not the effective clone, so the override user doesn't silently
    // get persisted back).
    cfg.update_server(&via_name, |s| {
        s.last_used = Some(chrono::Utc::now());
    })
    .context("updating server last_used")?;
    cfg.save().context("saving config after connect")?;

    println!(
        "tunnel up: 127.0.0.1:{} -> {}:{} (target {})",
        session.local_port(),
        effective_server.host,
        effective_server.relay_port,
        target_id
    );

    let viewer = Viewer::detect_with_override(cfg.general.viewer_override())
        .context("locating RustDesk viewer (spec §3.5)")?;
    tracing::debug!(viewer = ?viewer.kind(), "launching RustDesk viewer");

    let status = viewer
        .launch(&target_id)
        .context("spawning RustDesk viewer")?
        .wait()
        .context("waiting for RustDesk viewer to exit")?;

    session
        .close()
        .await
        .context("tearing down SSH session after viewer exit")?;

    if !status.success() {
        return Err(anyhow!(
            "RustDesk viewer exited with {status}; tunnel has been torn down"
        ));
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Target resolution
// -----------------------------------------------------------------------------

/// Resolve the user-supplied `<target>` argument to a validated
/// [`TargetId`] plus, when applicable, the matching registry [`Target`]
/// entry. Resolution order:
///
/// 1. Registry match by `id` (exact).
/// 2. Registry match by `label` (exact).
/// 3. Standalone [`TargetId::new`] — usable without persistence.
fn resolve_target(cfg: &Config, arg: &str) -> Result<(TargetId, Option<Target>)> {
    if let Ok(t) = cfg.get_target(arg) {
        let id = TargetId::new(&t.id).with_context(|| {
            format!(
                "target '{arg}' has stored id '{}' that fails spec §3.5",
                t.id
            )
        })?;
        return Ok((id, Some(t.clone())));
    }
    if let Some(t) = cfg
        .targets()
        .iter()
        .find(|t| t.label.as_deref() == Some(arg))
    {
        let id = TargetId::new(&t.id).with_context(|| {
            format!(
                "target with label '{arg}' has stored id '{}' that fails spec §3.5",
                t.id
            )
        })?;
        return Ok((id, Some(t.clone())));
    }
    let id = TargetId::new(arg).with_context(|| {
        format!(
            "'{arg}' is neither a registered target id/label nor a valid RustDesk ID (spec §3.5)"
        )
    })?;
    Ok((id, None))
}

fn resolve_via_name(
    via_cli: Option<&str>,
    matched: Option<&Target>,
    cfg: &Config,
) -> Result<String> {
    if let Some(v) = via_cli {
        return Ok(v.to_owned());
    }
    if let Some(t) = matched {
        if let Some(v) = &t.via_server {
            return Ok(v.clone());
        }
    }
    if let Some(d) = &cfg.general.default_server {
        return Ok(d.clone());
    }
    Err(anyhow!(
        "no relay server resolved: pass --via <server>, set target.via_server, \
         or set general.default_server"
    ))
}

// -----------------------------------------------------------------------------
// Credential resolution
// -----------------------------------------------------------------------------

async fn resolve_password(cfg: &Config, server: &RemoteServer) -> Result<Option<String>> {
    if has_default_ssh_keys() {
        tracing::debug!(
            "default SSH keys present; skipping password prompt (key auth will be tried first)"
        );
        return Ok(None);
    }
    let store = CredentialStore::from_config(cfg).context("building credential store")?;
    match store.get_password(&server.name, &server.ssh_user).await {
        Ok(pw) => Ok(Some(pw)),
        Err(RustmoteError::NoStoredCredential { .. }) => Ok(None),
        Err(e) => Err(anyhow::Error::from(e)),
    }
}

fn has_default_ssh_keys() -> bool {
    let Some(home) = home_dir() else {
        return false;
    };
    let ed = home.join(".ssh").join("id_ed25519");
    let rsa = home.join(".ssh").join("id_rsa");
    ed.is_file() || rsa.is_file()
}

fn home_dir() -> Option<PathBuf> {
    #[allow(deprecated)]
    std::env::home_dir()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn cfg_with(server: Option<&str>, targets: &[(&str, Option<&str>, Option<&str>)]) -> Config {
        let mut cfg = Config::default();
        if let Some(name) = server {
            cfg.add_server(RemoteServer::new(
                name,
                "10.0.0.1".parse::<IpAddr>().unwrap(),
                "charles",
                22,
                21_116,
            ))
            .unwrap();
        }
        for (id, label, via) in targets {
            let mut t = Target::new(*id);
            t.label = label.map(ToString::to_string);
            t.via_server = via.map(ToString::to_string);
            cfg.add_target(t).unwrap();
        }
        cfg
    }

    #[test]
    fn resolve_target_by_registered_id() {
        let cfg = cfg_with(None, &[("123456789", Some("voron"), Some("zima"))]);
        let (id, matched) = resolve_target(&cfg, "123456789").unwrap();
        assert_eq!(id.as_str(), "123456789");
        assert_eq!(matched.unwrap().label.as_deref(), Some("voron"));
    }

    #[test]
    fn resolve_target_by_label() {
        let cfg = cfg_with(None, &[("987654321", Some("voron"), None)]);
        let (id, matched) = resolve_target(&cfg, "voron").unwrap();
        assert_eq!(id.as_str(), "987654321");
        assert_eq!(matched.unwrap().id, "987654321");
    }

    #[test]
    fn resolve_target_standalone_valid_id() {
        let cfg = Config::default();
        let (id, matched) = resolve_target(&cfg, "123456789").unwrap();
        assert_eq!(id.as_str(), "123456789");
        assert!(matched.is_none());
    }

    #[test]
    fn resolve_target_rejects_unknown_label_and_bad_id() {
        let cfg = cfg_with(None, &[("123456789", Some("voron"), None)]);
        let err = resolve_target(&cfg, "not-a-known-label").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not-a-known-label"));
    }

    #[test]
    fn resolve_via_name_prefers_cli_flag() {
        let cfg = cfg_with(None, &[]);
        let name = resolve_via_name(Some("cli-server"), None, &cfg).unwrap();
        assert_eq!(name, "cli-server");
    }

    #[test]
    fn resolve_via_name_uses_target_via_server_next() {
        let cfg = cfg_with(None, &[]);
        let mut t = Target::new("123456789");
        t.via_server = Some("target-server".into());
        let name = resolve_via_name(None, Some(&t), &cfg).unwrap();
        assert_eq!(name, "target-server");
    }

    #[test]
    fn resolve_via_name_falls_back_to_default_server() {
        let mut cfg = Config::default();
        cfg.general.default_server = Some("default-server".into());
        let name = resolve_via_name(None, None, &cfg).unwrap();
        assert_eq!(name, "default-server");
    }

    #[test]
    fn resolve_via_name_errors_when_nothing_resolves() {
        let cfg = Config::default();
        assert!(resolve_via_name(None, None, &cfg).is_err());
    }
}
