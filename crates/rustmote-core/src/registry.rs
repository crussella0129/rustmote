//! `RemoteServer` registry CRUD.
//!
//! See `RUSTMOTE_SPEC.md` §3.1. The registry stores both [`RemoteServer`]
//! entries (keyed by `name`) and [`Target`] entries (keyed by `id`) inside
//! the on-disk [`crate::config::Config`]. The CRUD API lives as impls on
//! `Config` so the registry is always a consistent view of what was just
//! saved (or loaded) from disk.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::RustmoteError;
use crate::target::Target;

/// A registered relay server — an SSH jump host that also runs the
/// RustDesk relay (`hbbs`/`hbbr`).
///
/// The registry treats `name` as a case-sensitive primary key. Spec §6.4
/// restricts server names to `^[a-zA-Z0-9_-]{1,64}$`; callers building a
/// `RemoteServer` should validate before insertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteServer {
    /// Human-friendly unique name.
    pub name: String,

    /// Current LAN/WAN IP of the server. Hostname support is deferred to a
    /// later release per spec §3.1 ("IpAddr for v0.1").
    pub host: IpAddr,

    /// SSH port on the server. Default 22.
    pub ssh_port: u16,

    /// SSH username used for the tunnel.
    pub ssh_user: String,

    /// RustDesk `hbbs` port on the server. Default 21116.
    pub relay_port: u16,

    /// RustDesk relay public key (`data/id_ed25519.pub`). Populated by
    /// `rustmote relay bootstrap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_key: Option<String>,

    /// When this entry was added to the registry.
    pub created_at: DateTime<Utc>,

    /// When this entry was last touched by a successful connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<DateTime<Utc>>,
}

impl RemoteServer {
    /// Default SSH port when none is specified.
    pub const DEFAULT_SSH_PORT: u16 = 22;

    /// Default RustDesk `hbbs` port when none is specified.
    pub const DEFAULT_RELAY_PORT: u16 = 21116;

    /// Validate a server name against spec §6.4: 1-64 characters, each matching
    /// `[a-zA-Z0-9_-]`. This is the allowlist enforced at every registry
    /// insertion — names pass untrusted through the CLI into config storage,
    /// so validation is required before any `RemoteServer` is constructed from
    /// user input.
    ///
    /// # Errors
    /// Returns [`RustmoteError::InvalidServerName`] when the name is empty,
    /// longer than 64 chars, or contains any character outside the allowlist.
    pub fn validate_name(name: &str) -> crate::Result<()> {
        let len = name.len();
        if (1..=64).contains(&len)
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            Ok(())
        } else {
            Err(RustmoteError::InvalidServerName(name.to_owned()))
        }
    }

    /// Build a `RemoteServer` from the minimum set of fields, filling
    /// `created_at` with the current UTC time and `relay_key` / `last_used`
    /// with `None`.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        host: IpAddr,
        ssh_user: impl Into<String>,
        ssh_port: u16,
        relay_port: u16,
    ) -> Self {
        Self {
            name: name.into(),
            host,
            ssh_port,
            ssh_user: ssh_user.into(),
            relay_port,
            relay_key: None,
            created_at: Utc::now(),
            last_used: None,
        }
    }
}

// -----------------------------------------------------------------------------
// Registry operations (impls on Config).
//
// Keeping the API on `Config` means callers always see a consistent, typed
// view of the on-disk state. The CLI loads the config once, mutates via these
// methods, and saves once at the end of a command.
// -----------------------------------------------------------------------------

impl Config {
    /// Borrow the full server list.
    #[must_use]
    pub fn servers(&self) -> &[RemoteServer] {
        &self.servers
    }

    /// Borrow the full target list.
    #[must_use]
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// Look up a server by name.
    ///
    /// # Errors
    /// Returns [`RustmoteError::UnknownServer`] if no server with that name
    /// exists.
    pub fn get_server(&self, name: &str) -> crate::Result<&RemoteServer> {
        self.servers
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| RustmoteError::UnknownServer(name.to_owned()))
    }

    /// Add a new server to the registry.
    ///
    /// # Errors
    /// Returns [`RustmoteError::ServerAlreadyExists`] if a server with the
    /// same name is already registered.
    pub fn add_server(&mut self, server: RemoteServer) -> crate::Result<()> {
        if self.servers.iter().any(|s| s.name == server.name) {
            return Err(RustmoteError::ServerAlreadyExists(server.name));
        }
        self.servers.push(server);
        Ok(())
    }

    /// Remove a server by name, returning the removed entry.
    ///
    /// # Errors
    /// Returns [`RustmoteError::UnknownServer`] if no matching server exists.
    pub fn remove_server(&mut self, name: &str) -> crate::Result<RemoteServer> {
        let idx = self
            .servers
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| RustmoteError::UnknownServer(name.to_owned()))?;
        Ok(self.servers.remove(idx))
    }

    /// Update an existing server in place via a closure.
    ///
    /// # Errors
    /// Returns [`RustmoteError::UnknownServer`] if no matching server exists.
    pub fn update_server<F>(&mut self, name: &str, mutate: F) -> crate::Result<()>
    where
        F: FnOnce(&mut RemoteServer),
    {
        let server = self
            .servers
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| RustmoteError::UnknownServer(name.to_owned()))?;
        mutate(server);
        Ok(())
    }

    /// Look up a target by id.
    ///
    /// # Errors
    /// Returns [`RustmoteError::UnknownTarget`] if no target with that id
    /// exists.
    pub fn get_target(&self, id: &str) -> crate::Result<&Target> {
        self.targets
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| RustmoteError::UnknownTarget(id.to_owned()))
    }

    /// Add a target to the registry.
    ///
    /// # Errors
    /// Returns [`RustmoteError::TargetAlreadyExists`] on id collision.
    pub fn add_target(&mut self, target: Target) -> crate::Result<()> {
        if self.targets.iter().any(|t| t.id == target.id) {
            return Err(RustmoteError::TargetAlreadyExists(target.id));
        }
        self.targets.push(target);
        Ok(())
    }

    /// Remove a target by id, returning the removed entry.
    ///
    /// # Errors
    /// Returns [`RustmoteError::UnknownTarget`] if no matching target exists.
    pub fn remove_target(&mut self, id: &str) -> crate::Result<Target> {
        let idx = self
            .targets
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| RustmoteError::UnknownTarget(id.to_owned()))?;
        Ok(self.targets.remove(idx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_server(name: &str) -> RemoteServer {
        RemoteServer::new(
            name,
            "10.0.0.1".parse().unwrap(),
            "charles",
            RemoteServer::DEFAULT_SSH_PORT,
            RemoteServer::DEFAULT_RELAY_PORT,
        )
    }

    #[test]
    fn add_and_get_server() {
        let mut cfg = Config::default();
        cfg.add_server(sample_server("zima-brain")).unwrap();
        assert_eq!(cfg.servers().len(), 1);
        let s = cfg.get_server("zima-brain").unwrap();
        assert_eq!(s.ssh_user, "charles");
        assert_eq!(s.ssh_port, 22);
        assert_eq!(s.relay_port, 21116);
    }

    #[test]
    fn add_rejects_duplicate_name() {
        let mut cfg = Config::default();
        cfg.add_server(sample_server("zima-brain")).unwrap();
        let err = cfg.add_server(sample_server("zima-brain")).unwrap_err();
        assert!(matches!(err, RustmoteError::ServerAlreadyExists(name) if name == "zima-brain"));
    }

    #[test]
    fn get_unknown_errors() {
        let cfg = Config::default();
        let err = cfg.get_server("missing").unwrap_err();
        assert!(matches!(err, RustmoteError::UnknownServer(name) if name == "missing"));
    }

    #[test]
    fn remove_returns_entry_and_shrinks() {
        let mut cfg = Config::default();
        cfg.add_server(sample_server("a")).unwrap();
        cfg.add_server(sample_server("b")).unwrap();
        let removed = cfg.remove_server("a").unwrap();
        assert_eq!(removed.name, "a");
        assert_eq!(cfg.servers().len(), 1);
        assert_eq!(cfg.servers()[0].name, "b");
    }

    #[test]
    fn update_server_applies_closure() {
        let mut cfg = Config::default();
        cfg.add_server(sample_server("zima-brain")).unwrap();
        cfg.update_server("zima-brain", |s| {
            s.relay_key = Some("AAAA...".into());
            s.last_used = Some(Utc::now());
        })
        .unwrap();
        let s = cfg.get_server("zima-brain").unwrap();
        assert_eq!(s.relay_key.as_deref(), Some("AAAA..."));
        assert!(s.last_used.is_some());
    }

    #[test]
    fn validate_name_accepts_allowlist() {
        for ok in ["a", "zima-brain", "a_b_C-1", &"x".repeat(64)] {
            assert!(
                RemoteServer::validate_name(ok).is_ok(),
                "should accept {ok}"
            );
        }
    }

    #[test]
    fn validate_name_rejects_bad_input() {
        for bad in [
            "",
            &"x".repeat(65),
            "has space",
            "semi;colon",
            "dot.name",
            "slash/name",
            "emoji🦀",
            "tab\tname",
        ] {
            assert!(
                RemoteServer::validate_name(bad).is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn target_crud_works() {
        let mut cfg = Config::default();
        cfg.add_target(Target::new("123456789")).unwrap();
        assert_eq!(cfg.targets().len(), 1);
        let t = cfg.get_target("123456789").unwrap();
        assert_eq!(t.id, "123456789");

        let err = cfg.add_target(Target::new("123456789")).unwrap_err();
        assert!(matches!(err, RustmoteError::TargetAlreadyExists(_)));

        let removed = cfg.remove_target("123456789").unwrap();
        assert_eq!(removed.id, "123456789");
        assert!(cfg.targets().is_empty());
    }
}
