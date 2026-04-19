//! TOML config load/save and on-disk schema.
//!
//! See `RUSTMOTE_SPEC.md` §3.2. The config file lives at the OS-appropriate
//! location resolved via the [`directories`] crate:
//!
//! | OS      | Path                                                    |
//! |---------|---------------------------------------------------------|
//! | Linux   | `$XDG_CONFIG_HOME/rustmote/config.toml`                 |
//! | Windows | `%APPDATA%\rustmote\config.toml`                        |
//! | macOS   | `~/Library/Application Support/rustmote/config.toml`    |
//!
//! Plaintext credentials (only when `credential_mode = "unsafe"`) live in a
//! sibling file `credentials.toml` that must be mode `0600` on Unix. That
//! file is read/written by [`crate::credentials`] in Phase 3 (TASK-003);
//! this module deals only with `config.toml`.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::credentials::CredentialMode;
use crate::error::RustmoteError;
use crate::registry::RemoteServer;
use crate::target::Target;

/// File name of the main config, joined against the resolved config dir.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Sibling file name used by the `unsafe` credential mode. Managed by
/// [`crate::credentials`]; exported here so all path construction stays in
/// one module.
pub const CREDENTIALS_FILE_NAME: &str = "credentials.toml";

/// Top-level rustmote configuration persisted to disk.
///
/// The on-disk layout is the TOML schema documented in spec §3.2:
///
/// ```toml
/// [general]
/// credential_mode = "prompt"
/// default_server = "zima-brain"
/// viewer_path = ""
///
/// [[servers]]
/// name = "zima-brain"
/// # ...
///
/// [[targets]]
/// id = "123456789"
/// # ...
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,

    #[serde(default, rename = "servers", skip_serializing_if = "Vec::is_empty")]
    pub(crate) servers: Vec<RemoteServer>,

    #[serde(default, rename = "targets", skip_serializing_if = "Vec::is_empty")]
    pub(crate) targets: Vec<Target>,
}

/// `[general]` section of the config file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// How passwords are acquired. See spec §3.3.
    #[serde(default)]
    pub credential_mode: CredentialMode,

    /// Server used when `rustmote connect` is invoked without `--via`. `None`
    /// means "no default; require explicit --via".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_server: Option<String>,

    /// Override path to the RustDesk viewer binary. Empty string means
    /// auto-detect per spec §3.5.
    #[serde(default)]
    pub viewer_path: String,
}

impl GeneralConfig {
    /// Returns `Some(path)` if `viewer_path` is a non-empty override; `None`
    /// triggers auto-detection per spec §3.5.
    #[must_use]
    pub fn viewer_override(&self) -> Option<&Path> {
        if self.viewer_path.is_empty() {
            None
        } else {
            Some(Path::new(&self.viewer_path))
        }
    }
}

/// Returns the rustmote config directory (creating it is the caller's job).
///
/// # Errors
/// Returns [`RustmoteError::NoConfigDir`] on exotic platforms where
/// [`ProjectDirs`] cannot locate a user config base directory.
pub fn config_dir() -> crate::Result<PathBuf> {
    ProjectDirs::from("", "", "rustmote")
        .map(|p| p.config_dir().to_path_buf())
        .ok_or(RustmoteError::NoConfigDir)
}

/// Returns the fully-qualified path to `config.toml`.
///
/// # Errors
/// Propagates [`config_dir`] failures.
pub fn config_path() -> crate::Result<PathBuf> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

/// Returns the fully-qualified path to `credentials.toml`.
///
/// # Errors
/// Propagates [`config_dir`] failures.
pub fn credentials_path() -> crate::Result<PathBuf> {
    Ok(config_dir()?.join(CREDENTIALS_FILE_NAME))
}

impl Config {
    /// Load the config from the OS-appropriate path.
    ///
    /// Missing file → [`Config::default`] (not an error); first call is how
    /// `rustmote server add` bootstraps onto a fresh machine.
    ///
    /// # Errors
    /// Returns [`RustmoteError::ConfigParse`] if the file exists but cannot
    /// be parsed; I/O errors propagate as [`RustmoteError::Io`].
    pub fn load() -> crate::Result<Self> {
        Self::load_from(&config_path()?)
    }

    /// Load the config from an explicit path. Missing file → default.
    ///
    /// # Errors
    /// Parse errors and I/O errors other than `NotFound` are propagated.
    pub fn load_from(path: &Path) -> crate::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str(&raw).map_err(|e| RustmoteError::ConfigParse {
                path: path.to_path_buf(),
                source: e,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(RustmoteError::Io(err)),
        }
    }

    /// Save the config to the OS-appropriate path, creating the parent
    /// directory if necessary. Writes atomically via a sibling temp file and
    /// rename to avoid partial writes if the process is killed mid-save.
    ///
    /// # Errors
    /// I/O and serialization errors propagate.
    pub fn save(&self) -> crate::Result<()> {
        self.save_to(&config_path()?)
    }

    /// Save the config to an explicit path. Atomic (temp file + rename) and
    /// creates any missing parent directories.
    ///
    /// # Errors
    /// I/O and serialization errors propagate.
    pub fn save_to(&self, path: &Path) -> crate::Result<()> {
        let serialized = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = tmp_path(path);
        std::fs::write(&tmp, serialized.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn scratch(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "rustmote-test-{}-{}-{}",
            name,
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ));
        p.push("config.toml");
        p
    }

    #[test]
    fn default_is_empty() {
        let cfg = Config::default();
        assert!(cfg.servers().is_empty());
        assert!(cfg.targets().is_empty());
        assert_eq!(cfg.general.credential_mode, CredentialMode::Prompt);
        assert!(cfg.general.default_server.is_none());
        assert!(cfg.general.viewer_path.is_empty());
    }

    #[test]
    fn load_missing_returns_default() {
        let path = scratch("missing");
        assert!(!path.exists());
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn save_creates_parent_dir_and_is_atomic() {
        let path = scratch("save-atomic");
        let parent = path.parent().unwrap().to_path_buf();
        assert!(!parent.exists(), "scratch dir must not pre-exist");

        let cfg = Config::default();
        cfg.save_to(&path).unwrap();
        assert!(path.exists());
        // Temp file must be gone after rename.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(!PathBuf::from(tmp).exists());

        std::fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn roundtrip_populated_config() {
        let path = scratch("roundtrip");

        let mut cfg = Config::default();
        cfg.general.credential_mode = CredentialMode::Keychain;
        cfg.general.default_server = Some("zima-brain".into());
        cfg.general.viewer_path = "/opt/rustdesk/rustdesk".into();

        cfg.add_server(RemoteServer::new(
            "zima-brain",
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            "charles",
            22,
            21_116,
        ))
        .unwrap();
        cfg.add_target(Target::new("123456789")).unwrap();

        cfg.save_to(&path).unwrap();
        let reloaded = Config::load_from(&path).unwrap();
        assert_eq!(cfg, reloaded);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn parse_error_surfaces_the_path() {
        let path = scratch("bad-toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not [valid toml").unwrap();

        let err = Config::load_from(&path).unwrap_err();
        match err {
            RustmoteError::ConfigParse { path: p, .. } => assert_eq!(p, path),
            other => panic!("unexpected error: {other:?}"),
        }

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn viewer_override_empty_is_none() {
        let g = GeneralConfig::default();
        assert!(g.viewer_override().is_none());
    }

    #[test]
    fn viewer_override_nonempty_returns_path() {
        let g = GeneralConfig {
            viewer_path: "/opt/rustdesk/rustdesk".into(),
            ..GeneralConfig::default()
        };
        assert_eq!(
            g.viewer_override(),
            Some(Path::new("/opt/rustdesk/rustdesk"))
        );
    }
}
