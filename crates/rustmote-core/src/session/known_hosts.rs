//! Trust-on-first-use host-key store per spec §6.7.
//!
//! Persisted at `$CONFIG/rustmote/known_hosts.toml` alongside the main
//! config (see `config::known_hosts_path`). The schema is intentionally
//! flat and TOML-friendly so an operator can audit it without running
//! `rustmote`:
//!
//! ```toml
//! [entries."zima-brain.local:22"]
//! fingerprint = "SHA256:uKzM…"
//! key_type = "ssh-ed25519"
//! first_seen = "2026-04-19T00:00:00Z"
//! ```
//!
//! The library never prompts on mismatch — that UX lives in
//! `rustmote-cli`. This module returns a structured [`TofuOutcome`] and
//! leaves the decision to the caller.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::config_dir;
use crate::error::RustmoteError;

/// File name used inside the rustmote config dir.
pub const KNOWN_HOSTS_FILE_NAME: &str = "known_hosts.toml";

/// Resolve the absolute path to `known_hosts.toml`.
///
/// # Errors
/// Returns [`RustmoteError::NoConfigDir`] if the OS config directory
/// cannot be resolved.
pub fn known_hosts_path() -> crate::Result<PathBuf> {
    Ok(config_dir()?.join(KNOWN_HOSTS_FILE_NAME))
}

/// A single pinned host-key entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKey {
    /// Base-64 (no pad) SHA-256 of the server public key, as produced by
    /// `russh_keys::key::PublicKey::fingerprint`, prefixed with
    /// `SHA256:` so it matches the OpenSSH presentation operators
    /// already know.
    pub fingerprint: String,

    /// SSH key type string (`ssh-ed25519`, `ssh-rsa`, `ecdsa-…`).
    pub key_type: String,

    /// When this entry was first pinned.
    pub first_seen: DateTime<Utc>,
}

/// On-disk TOFU store.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHosts {
    /// Keyed by `"{host}:{port}"` — matching the SSH connect string.
    #[serde(default)]
    pub entries: BTreeMap<String, HostKey>,
}

/// Result of a TOFU check.
///
/// On [`TofuOutcome::Pinned`] the caller is responsible for persisting
/// the store. [`TofuOutcome::Mismatch`] is never implicitly accepted —
/// the caller must surface a fatal error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TofuOutcome {
    /// New host entry was just pinned; caller must `save()`.
    Pinned,
    /// Host already known and fingerprint matched.
    Matched,
    /// Host known but fingerprint differs — refuse.
    Mismatch {
        expected: HostKey,
        actual_fingerprint: String,
    },
    /// Host not known and the caller refused to pin (strict mode).
    UnknownRejected,
}

/// Whether an unknown host may be silently pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TofuPolicy {
    /// Auto-pin on first use. The standard TOFU behavior.
    #[default]
    TrustOnFirstUse,

    /// Never pin implicitly — an unknown host is a hard refusal. The CLI
    /// uses this when the operator has opted in to strict mode.
    Strict,
}

impl KnownHosts {
    /// Load the store from the spec-default path. A missing file yields an
    /// empty store — first-use pinning will populate it.
    ///
    /// # Errors
    /// I/O errors other than "file not found" propagate. TOML parse
    /// failures surface as [`RustmoteError::ConfigParse`].
    pub fn load() -> crate::Result<Self> {
        Self::load_from(known_hosts_path()?)
    }

    /// Load from an explicit path (used by tests and by the CLI when it
    /// has already resolved the path).
    ///
    /// # Errors
    /// See [`Self::load`].
    pub fn load_from(path: impl AsRef<Path>) -> crate::Result<Self> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str(&raw).map_err(|e| RustmoteError::ConfigParse {
                path: path.to_path_buf(),
                source: e,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(RustmoteError::Io(e)),
        }
    }

    /// Save the store atomically (temp-file + rename) to the spec-default
    /// path.
    ///
    /// # Errors
    /// I/O and TOML serialization errors propagate.
    pub fn save(&self) -> crate::Result<()> {
        self.save_to(known_hosts_path()?)
    }

    /// Save the store to an explicit path.
    ///
    /// # Errors
    /// See [`Self::save`].
    pub fn save_to(&self, path: impl AsRef<Path>) -> crate::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, serialized)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Endpoint key used in the map: `"{host}:{port}"`.
    #[must_use]
    pub fn endpoint(host: &str, port: u16) -> String {
        format!("{host}:{port}")
    }

    /// Borrow a pinned entry if present.
    #[must_use]
    pub fn get(&self, host: &str, port: u16) -> Option<&HostKey> {
        self.entries.get(&Self::endpoint(host, port))
    }

    /// Verify an observed fingerprint against the store.
    ///
    /// - If the endpoint is unknown and `policy == TrustOnFirstUse`: pin
    ///   it and return [`TofuOutcome::Pinned`]. The caller must call
    ///   [`Self::save`] (or [`Self::save_to`]) to persist.
    /// - If the endpoint is unknown and `policy == Strict`: return
    ///   [`TofuOutcome::UnknownRejected`].
    /// - If known and matches: [`TofuOutcome::Matched`].
    /// - If known and differs: [`TofuOutcome::Mismatch { .. }`].
    pub fn verify_or_pin(
        &mut self,
        host: &str,
        port: u16,
        observed: &HostKey,
        policy: TofuPolicy,
    ) -> TofuOutcome {
        let key = Self::endpoint(host, port);
        if let Some(existing) = self.entries.get(&key) {
            if existing.fingerprint == observed.fingerprint {
                TofuOutcome::Matched
            } else {
                TofuOutcome::Mismatch {
                    expected: existing.clone(),
                    actual_fingerprint: observed.fingerprint.clone(),
                }
            }
        } else {
            match policy {
                TofuPolicy::TrustOnFirstUse => {
                    self.entries.insert(key, observed.clone());
                    TofuOutcome::Pinned
                }
                TofuPolicy::Strict => TofuOutcome::UnknownRejected,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_key(fp: &str) -> HostKey {
        HostKey {
            fingerprint: format!("SHA256:{fp}"),
            key_type: "ssh-ed25519".into(),
            first_seen: Utc::now(),
        }
    }

    #[test]
    fn endpoint_formats_host_and_port() {
        assert_eq!(
            KnownHosts::endpoint("zima-brain.local", 22),
            "zima-brain.local:22"
        );
    }

    #[test]
    fn first_use_pins_on_trust_policy() {
        let mut kh = KnownHosts::default();
        let outcome = kh.verify_or_pin("zima", 22, &fake_key("abc"), TofuPolicy::TrustOnFirstUse);
        assert_eq!(outcome, TofuOutcome::Pinned);
        assert!(kh.get("zima", 22).is_some());
    }

    #[test]
    fn first_use_rejects_on_strict_policy() {
        let mut kh = KnownHosts::default();
        let outcome = kh.verify_or_pin("zima", 22, &fake_key("abc"), TofuPolicy::Strict);
        assert_eq!(outcome, TofuOutcome::UnknownRejected);
        assert!(kh.get("zima", 22).is_none());
    }

    #[test]
    fn matching_fingerprint_accepts() {
        let mut kh = KnownHosts::default();
        let observed = fake_key("abc");
        assert_eq!(
            kh.verify_or_pin("zima", 22, &observed, TofuPolicy::TrustOnFirstUse),
            TofuOutcome::Pinned
        );
        // Second call with the same fingerprint should match.
        let outcome = kh.verify_or_pin("zima", 22, &observed, TofuPolicy::Strict);
        assert_eq!(outcome, TofuOutcome::Matched);
    }

    #[test]
    fn mismatched_fingerprint_refuses() {
        let mut kh = KnownHosts::default();
        kh.verify_or_pin("zima", 22, &fake_key("abc"), TofuPolicy::TrustOnFirstUse);

        let outcome = kh.verify_or_pin(
            "zima",
            22,
            &fake_key("deadbeef"),
            TofuPolicy::TrustOnFirstUse,
        );
        match outcome {
            TofuOutcome::Mismatch {
                expected,
                actual_fingerprint,
            } => {
                assert_eq!(expected.fingerprint, "SHA256:abc");
                assert_eq!(actual_fingerprint, "SHA256:deadbeef");
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let mut kh = KnownHosts::default();
        kh.verify_or_pin("zima", 22, &fake_key("abc"), TofuPolicy::TrustOnFirstUse);
        kh.verify_or_pin(
            "pi.lan",
            2222,
            &fake_key("xyz"),
            TofuPolicy::TrustOnFirstUse,
        );

        let dir = std::env::temp_dir().join(format!(
            "rustmote-kh-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(KNOWN_HOSTS_FILE_NAME);
        kh.save_to(&path).unwrap();

        let loaded = KnownHosts::load_from(&path).unwrap();
        assert_eq!(loaded, kh);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_missing_file_is_default() {
        let path = std::env::temp_dir().join(format!(
            "rustmote-kh-missing-{}-{}.toml",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        assert!(!path.exists());
        let kh = KnownHosts::load_from(&path).unwrap();
        assert!(kh.entries.is_empty());
    }
}
