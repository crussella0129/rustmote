//! On-server `.rustmote-state.toml` schema (spec §5.1.1).
//!
//! The state file is the source of truth for what versions are currently
//! deployed on a relay host. `relay_lifecycle` reads it on every command,
//! mutates it on `bootstrap` and `update`, and preserves the prior copy
//! under `backups/pre-update-<iso>/` so that a rollback can restore it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::RustmoteError;

/// Top-level `.rustmote-state.toml` structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayState {
    pub install: InstallMetadata,

    #[serde(default, rename = "images")]
    pub images: Vec<ImagePin>,
}

/// `[install]` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallMetadata {
    pub bootstrapped_at: DateTime<Utc>,
    pub bootstrapped_by_rustmote_version: String,

    /// Last successful update timestamp. `None` on a freshly bootstrapped
    /// relay until the first `update` completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated_at: Option<DateTime<Utc>>,
}

/// A single `[[images]]` entry. One per service in the compose file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePin {
    pub service: String,
    pub repo: String,
    pub tag: String,
    pub digest: String,
    pub pinned_at: DateTime<Utc>,
}

impl RelayState {
    /// Construct a fresh state for a just-completed bootstrap.
    #[must_use]
    pub fn new_bootstrap(
        at: DateTime<Utc>,
        rustmote_version: impl Into<String>,
        images: Vec<ImagePin>,
    ) -> Self {
        Self {
            install: InstallMetadata {
                bootstrapped_at: at,
                bootstrapped_by_rustmote_version: rustmote_version.into(),
                last_updated_at: None,
            },
            images,
        }
    }

    /// Parse a TOML string (the bytes returned from reading the remote
    /// state file).
    ///
    /// # Errors
    /// Returns [`RustmoteError::ConfigParse`] on malformed TOML. The
    /// `path` embedded in the error is the conventional remote path
    /// `/opt/rustmote-relay/.rustmote-state.toml` (or whatever caller
    /// passes); used only for diagnostic context.
    pub fn from_toml_str(raw: &str, source_hint: &str) -> crate::Result<Self> {
        toml::from_str(raw).map_err(|e| RustmoteError::ConfigParse {
            path: std::path::PathBuf::from(source_hint),
            source: e,
        })
    }

    /// Serialize to pretty TOML for writing back to the remote.
    ///
    /// # Errors
    /// Propagates TOML serialization failures.
    pub fn to_toml_string(&self) -> crate::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Replace `images` with `new_images` and stamp `last_updated_at`.
    pub fn apply_update(&mut self, new_images: Vec<ImagePin>, at: DateTime<Utc>) {
        self.images = new_images;
        self.install.last_updated_at = Some(at);
    }

    /// Lookup the pin for `service`, if any.
    #[must_use]
    pub fn pin_for(&self, service: &str) -> Option<&ImagePin> {
        self.images.iter().find(|p| p.service == service)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-04-18T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn bootstrap_roundtrips_through_toml_with_two_services() {
        let images = vec![
            ImagePin {
                service: "hbbs".into(),
                repo: "rustdesk/rustdesk-server".into(),
                tag: "1.1.11".into(),
                digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .into(),
                pinned_at: ts(),
            },
            ImagePin {
                service: "hbbr".into(),
                repo: "rustdesk/rustdesk-server".into(),
                tag: "1.1.11".into(),
                digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .into(),
                pinned_at: ts(),
            },
        ];
        let state = RelayState::new_bootstrap(ts(), "0.1.0", images.clone());

        let s = state.to_toml_string().unwrap();
        let parsed =
            RelayState::from_toml_str(&s, "/opt/rustmote-relay/.rustmote-state.toml").unwrap();
        assert_eq!(parsed, state);
        assert_eq!(parsed.install.last_updated_at, None);
        assert_eq!(parsed.images, images);
    }

    #[test]
    fn apply_update_bumps_images_and_stamps_last_updated_at() {
        let mut state = RelayState::new_bootstrap(ts(), "0.1.0", vec![]);
        let later = ts() + chrono::Duration::hours(1);
        let new = vec![ImagePin {
            service: "hbbs".into(),
            repo: "rustdesk/rustdesk-server".into(),
            tag: "1.1.14".into(),
            digest: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                .into(),
            pinned_at: later,
        }];
        state.apply_update(new.clone(), later);
        assert_eq!(state.install.last_updated_at, Some(later));
        assert_eq!(state.images, new);
    }

    #[test]
    fn pin_for_returns_match_or_none() {
        let state = RelayState::new_bootstrap(
            ts(),
            "0.1.0",
            vec![ImagePin {
                service: "hbbs".into(),
                repo: "rustdesk/rustdesk-server".into(),
                tag: "1.1.11".into(),
                digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .into(),
                pinned_at: ts(),
            }],
        );
        assert!(state.pin_for("hbbs").is_some());
        assert!(state.pin_for("unknown").is_none());
    }

    #[test]
    fn malformed_toml_surfaces_the_hint_path() {
        let err =
            RelayState::from_toml_str("not [[valid", "/opt/rustmote-relay/.rustmote-state.toml")
                .unwrap_err();
        match err {
            RustmoteError::ConfigParse { path, .. } => {
                assert_eq!(
                    path.to_str(),
                    Some("/opt/rustmote-relay/.rustmote-state.toml")
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
