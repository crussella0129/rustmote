//! `Target` type — a machine reachable through a relay.
//!
//! See `RUSTMOTE_SPEC.md` §3.1. LAN discovery utilities live in
//! [`crate::discovery`] (Phase 6 / TASK-006).

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A target machine reachable through a registered relay server.
///
/// Identified primarily by its RustDesk ID (a 9–10 digit string) or by an
/// explicit label. The LAN `ip` is cached from the last successful discovery
/// but is never authoritative — connections are always made through the
/// tunneled relay endpoint, not the IP directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// RustDesk ID (matches `^[0-9]{9,10}$`) or a user-chosen label.
    pub id: String,

    /// Last known LAN IP from discovery. Informational only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<IpAddr>,

    /// User-assigned friendly name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Name of the `RemoteServer` this target should be reached through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_server: Option<String>,

    /// Last time this target appeared in a discovery sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<DateTime<Utc>>,
}

impl Target {
    /// Construct a new target with just an identifier; other fields default
    /// to `None`.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ip: None,
            label: None,
            via_server: None,
            last_seen: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_populates_only_id() {
        let t = Target::new("123456789");
        assert_eq!(t.id, "123456789");
        assert!(
            t.ip.is_none() && t.label.is_none() && t.via_server.is_none() && t.last_seen.is_none()
        );
    }

    #[test]
    fn serde_toml_roundtrip_minimal() {
        let t = Target::new("123456789");
        let s = toml::to_string(&t).unwrap();
        let back: Target = toml::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn serde_toml_roundtrip_populated() {
        let t = Target {
            id: "987654321".into(),
            ip: Some("10.0.0.42".parse().unwrap()),
            label: Some("voron-controller".into()),
            via_server: Some("zima-brain".into()),
            last_seen: None,
        };
        let s = toml::to_string(&t).unwrap();
        let back: Target = toml::from_str(&s).unwrap();
        assert_eq!(t, back);
    }
}
