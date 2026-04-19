//! Three-tier credential handling: prompt / keychain / unsafe.
//!
//! Full dispatch implementation lands in Phase 3 (TASK-003) per spec §3.3.
//! Phase 2 (TASK-002) defines only the [`CredentialMode`] enum because it is
//! referenced by the on-disk [`crate::config::Config`] schema.

use serde::{Deserialize, Serialize};

/// How `rustmote` acquires passwords for SSH authentication.
///
/// See `RUSTMOTE_SPEC.md` §3.3 and §6.1–§6.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CredentialMode {
    /// Ask every time via `rpassword`. Never persists credentials. Default.
    #[default]
    Prompt,

    /// Store in the OS keyring via the `keyring` crate. Service name
    /// `rustmote`; account format `"{server_name}:{username}"`.
    Keychain,

    /// Plaintext in `$CONFIG/rustmote/credentials.toml` (mode `0600` on Unix).
    /// Requires explicit user acknowledgment via
    /// `rustmote config set-mode unsafe --i-understand-this-is-insecure`.
    Unsafe,
}

impl CredentialMode {
    /// Human-readable identifier; matches the TOML serialized form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Keychain => "keychain",
            Self::Unsafe => "unsafe",
        }
    }
}

impl std::fmt::Display for CredentialMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for CredentialMode {
    type Err = UnknownCredentialMode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "prompt" => Ok(Self::Prompt),
            "keychain" => Ok(Self::Keychain),
            "unsafe" => Ok(Self::Unsafe),
            other => Err(UnknownCredentialMode(other.to_owned())),
        }
    }
}

/// Error returned when a credential-mode string fails to parse.
#[derive(Debug, thiserror::Error)]
#[error("unknown credential mode '{0}'; expected one of: prompt | keychain | unsafe")]
pub struct UnknownCredentialMode(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn default_is_prompt() {
        assert_eq!(CredentialMode::default(), CredentialMode::Prompt);
    }

    #[test]
    fn display_roundtrips_through_fromstr() {
        for mode in [
            CredentialMode::Prompt,
            CredentialMode::Keychain,
            CredentialMode::Unsafe,
        ] {
            let s = mode.to_string();
            let parsed = CredentialMode::from_str(&s).expect("roundtrip");
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn fromstr_is_case_insensitive() {
        assert_eq!(
            CredentialMode::from_str("PROMPT").unwrap(),
            CredentialMode::Prompt
        );
        assert_eq!(
            CredentialMode::from_str("  KeyChain  ").unwrap(),
            CredentialMode::Keychain
        );
    }

    #[test]
    fn fromstr_rejects_unknown() {
        assert!(CredentialMode::from_str("bogus").is_err());
    }

    #[test]
    fn serde_json_uses_lowercase() {
        let json = serde_json::to_string(&CredentialMode::Keychain).unwrap();
        assert_eq!(json, "\"keychain\"");
        let parsed: CredentialMode = serde_json::from_str("\"unsafe\"").unwrap();
        assert_eq!(parsed, CredentialMode::Unsafe);
    }
}
