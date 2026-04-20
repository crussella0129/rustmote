//! Three-tier credential handling: prompt / keychain / unsafe.
//!
//! Implements `RUSTMOTE_SPEC.md` §3.3 (dispatch) and §6.1–§6.3 (security):
//!
//! - **Prompt** (default): `rpassword::prompt_password` every call. `set`
//!   and `delete` are no-ops.
//! - **Keychain**: OS keyring via the [`keyring`] crate. Service name
//!   `"rustmote"`, account format `"{server}:{user}"`.
//! - **Unsafe**: plaintext TOML at `$CONFIG/rustmote/credentials.toml`.
//!   On Unix the file **must** be mode `0600`; wider permissions are a
//!   hard refusal per §6.3. Every access logs a `tracing::warn!`. On
//!   non-Unix platforms unsafe mode is refused until ACL verification
//!   lands — users should prefer keychain mode (Windows Credential
//!   Manager / macOS Keychain).
//!
//! The CLI gate requiring `--i-understand-this-is-insecure` on first
//! enable lives in `rustmote-cli` (§6.2) — this module is concerned
//! only with on-disk / keyring mechanics.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{credentials_path, Config};
use crate::error::RustmoteError;

/// Service name used in the OS keyring per spec §3.3.
pub const KEYRING_SERVICE: &str = "rustmote";

// -----------------------------------------------------------------------------
// CredentialMode enum (unchanged from Phase 2)
// -----------------------------------------------------------------------------

/// How `rustmote` acquires passwords for SSH authentication.
///
/// See `RUSTMOTE_SPEC.md` §3.3 and §6.1–§6.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CredentialMode {
    /// Ask every time via `rpassword`. Never persists credentials. Default.
    #[default]
    Prompt,

    /// Store in the OS keyring via the `keyring` crate.
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

// -----------------------------------------------------------------------------
// KeyringBackend trait (DI seam for testing; spec §7.2 mock keyring)
// -----------------------------------------------------------------------------

/// Minimal abstraction over the OS keyring. Real code uses
/// [`SystemKeyring`]; tests substitute an in-memory implementation.
///
/// Methods are synchronous because the underlying `keyring` crate is
/// synchronous; the [`CredentialStore`] layer wraps these calls in
/// [`tokio::task::spawn_blocking`] so they do not stall the async runtime.
pub trait KeyringBackend: Send + Sync + 'static {
    /// Retrieve the stored password, or [`RustmoteError::NoStoredCredential`]
    /// if the entry does not exist.
    fn get(&self, service: &str, account: &str) -> crate::Result<String>;

    /// Store or overwrite a password for `(service, account)`.
    fn set(&self, service: &str, account: &str, password: &str) -> crate::Result<()>;

    /// Remove the entry; a missing entry is treated as success (idempotent).
    fn delete(&self, service: &str, account: &str) -> crate::Result<()>;
}

/// Real OS keyring backend backed by the [`keyring`] crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemKeyring;

impl KeyringBackend for SystemKeyring {
    fn get(&self, service: &str, account: &str) -> crate::Result<String> {
        let entry = keyring::Entry::new(service, account)?;
        match entry.get_password() {
            Ok(p) => Ok(p),
            Err(keyring::Error::NoEntry) => Err(RustmoteError::NoStoredCredential {
                server: split_account_server(account).to_owned(),
                user: split_account_user(account).to_owned(),
            }),
            Err(e) => Err(RustmoteError::Credential(e)),
        }
    }

    fn set(&self, service: &str, account: &str, password: &str) -> crate::Result<()> {
        let entry = keyring::Entry::new(service, account)?;
        entry.set_password(password).map_err(Into::into)
    }

    fn delete(&self, service: &str, account: &str) -> crate::Result<()> {
        let entry = keyring::Entry::new(service, account)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(RustmoteError::Credential(e)),
        }
    }
}

fn keyring_account(server: &str, user: &str) -> String {
    format!("{server}:{user}")
}

fn split_account_server(account: &str) -> &str {
    account.split_once(':').map_or(account, |(s, _)| s)
}

fn split_account_user(account: &str) -> &str {
    account.split_once(':').map_or("", |(_, u)| u)
}

// -----------------------------------------------------------------------------
// Unsafe file format
// -----------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct UnsafeFile {
    /// Keyed by `"{server}:{user}"` to match the keyring account format.
    #[serde(default)]
    passwords: BTreeMap<String, String>,
}

// -----------------------------------------------------------------------------
// CredentialStore — the resolved dispatcher
// -----------------------------------------------------------------------------

/// Resolved credential backend for a single operation.
///
/// Built via [`CredentialStore::from_mode`] (production) or one of the
/// explicit constructors ([`Self::prompt`], [`Self::with_keyring`],
/// [`Self::with_unsafe_file`]) used by tests.
pub enum CredentialStore {
    /// Interactive `rpassword` prompt; never persists.
    Prompt,
    /// Keyring-backed, via any type implementing [`KeyringBackend`]. Held
    /// as an [`Arc`] so blocking calls can be moved into
    /// [`tokio::task::spawn_blocking`] by value without lifetime acrobatics.
    Keychain(Arc<dyn KeyringBackend>),
    /// Plaintext TOML at the given path. Path is typically
    /// [`credentials_path`] but tests use scratch paths.
    Unsafe { path: PathBuf },
}

impl CredentialStore {
    /// Build a store for the given credential mode using production
    /// backends: `SystemKeyring` for `Keychain`, the OS-appropriate
    /// [`credentials_path`] for `Unsafe`.
    ///
    /// # Errors
    /// Returns [`RustmoteError::NoConfigDir`] on exotic platforms where
    /// the unsafe-file path cannot be resolved.
    pub fn from_mode(mode: CredentialMode) -> crate::Result<Self> {
        match mode {
            CredentialMode::Prompt => Ok(Self::Prompt),
            CredentialMode::Keychain => Ok(Self::with_keyring(SystemKeyring)),
            CredentialMode::Unsafe => Ok(Self::Unsafe {
                path: credentials_path()?,
            }),
        }
    }

    /// Build a store from the already-loaded [`Config`], using the same
    /// dispatch as [`Self::from_mode`].
    ///
    /// # Errors
    /// Same as [`Self::from_mode`].
    pub fn from_config(cfg: &Config) -> crate::Result<Self> {
        Self::from_mode(cfg.general.credential_mode)
    }

    /// Prompt-mode store (no persistence).
    #[must_use]
    pub fn prompt() -> Self {
        Self::Prompt
    }

    /// Keychain-mode store with an injectable backend.
    pub fn with_keyring<K: KeyringBackend>(backend: K) -> Self {
        Self::Keychain(Arc::new(backend))
    }

    /// Keychain-mode store from an already-shared backend (useful when
    /// multiple stores should observe the same in-memory mock in tests).
    #[must_use]
    pub fn with_shared_keyring(backend: Arc<dyn KeyringBackend>) -> Self {
        Self::Keychain(backend)
    }

    /// Unsafe-mode store at an explicit path (tests, or when the CLI has
    /// resolved the path itself).
    pub fn with_unsafe_file(path: impl Into<PathBuf>) -> Self {
        Self::Unsafe { path: path.into() }
    }

    /// Retrieve a password.
    ///
    /// # Errors
    /// - Prompt: I/O error from the terminal.
    /// - Keychain: backend error or [`RustmoteError::NoStoredCredential`]
    ///   if the entry does not exist.
    /// - Unsafe: [`RustmoteError::InsecureCredentialsFile`] if permissions
    ///   are wider than `0600` (Unix), parse errors on malformed TOML, or
    ///   [`RustmoteError::NoStoredCredential`] if the entry is absent.
    pub async fn get_password(&self, server: &str, user: &str) -> crate::Result<String> {
        match self {
            Self::Prompt => prompt_for_password(server, user).await,
            Self::Keychain(backend) => {
                let backend = Arc::clone(backend);
                let service = KEYRING_SERVICE;
                let account = keyring_account(server, user);
                tokio::task::spawn_blocking(move || backend.get(service, &account))
                    .await
                    .map_err(|e| io_from_join(&e))?
            }
            Self::Unsafe { path } => {
                tracing::warn!(
                    server = %server,
                    user = %user,
                    "credential read via UNSAFE plaintext file (see spec §6.3)"
                );
                let file = read_unsafe_file(path).await?;
                file.passwords
                    .get(&keyring_account(server, user))
                    .cloned()
                    .ok_or_else(|| RustmoteError::NoStoredCredential {
                        server: server.to_owned(),
                        user: user.to_owned(),
                    })
            }
        }
    }

    /// Store a password.
    ///
    /// Prompt mode is a no-op that returns `Ok` per spec §3.3. Keychain
    /// and Unsafe modes persist the password.
    ///
    /// # Errors
    /// Keychain/Unsafe backend errors propagate.
    pub async fn set_password(
        &self,
        server: &str,
        user: &str,
        password: &str,
    ) -> crate::Result<()> {
        match self {
            Self::Prompt => {
                tracing::debug!(
                    server = %server,
                    user = %user,
                    "prompt-mode set_password is a no-op"
                );
                Ok(())
            }
            Self::Keychain(backend) => {
                let backend = Arc::clone(backend);
                let service = KEYRING_SERVICE;
                let account = keyring_account(server, user);
                let password = password.to_owned();
                tokio::task::spawn_blocking(move || backend.set(service, &account, &password))
                    .await
                    .map_err(|e| io_from_join(&e))?
            }
            Self::Unsafe { path } => {
                tracing::warn!(
                    server = %server,
                    user = %user,
                    "credential write via UNSAFE plaintext file (see spec §6.3)"
                );
                let mut file = read_unsafe_file(path).await?;
                file.passwords
                    .insert(keyring_account(server, user), password.to_owned());
                write_unsafe_file(path, &file).await
            }
        }
    }

    /// Remove a stored password. Missing entries are treated as success.
    ///
    /// # Errors
    /// Backend errors propagate. Prompt mode is a no-op.
    pub async fn delete_password(&self, server: &str, user: &str) -> crate::Result<()> {
        match self {
            Self::Prompt => Ok(()),
            Self::Keychain(backend) => {
                let backend = Arc::clone(backend);
                let service = KEYRING_SERVICE;
                let account = keyring_account(server, user);
                tokio::task::spawn_blocking(move || backend.delete(service, &account))
                    .await
                    .map_err(|e| io_from_join(&e))?
            }
            Self::Unsafe { path } => {
                tracing::warn!(
                    server = %server,
                    user = %user,
                    "credential delete via UNSAFE plaintext file (see spec §6.3)"
                );
                let mut file = read_unsafe_file(path).await?;
                file.passwords.remove(&keyring_account(server, user));
                write_unsafe_file(path, &file).await
            }
        }
    }
}

fn io_from_join(e: &tokio::task::JoinError) -> RustmoteError {
    RustmoteError::Io(io::Error::other(format!("credential task join: {e}")))
}

// -----------------------------------------------------------------------------
// Top-level convenience functions — spec §3.3 signatures
// -----------------------------------------------------------------------------

/// Retrieve a password using the credential mode configured on disk.
///
/// # Errors
/// See [`CredentialStore::get_password`].
pub async fn get_password(server_name: &str, username: &str) -> crate::Result<String> {
    let cfg = Config::load()?;
    CredentialStore::from_config(&cfg)?
        .get_password(server_name, username)
        .await
}

/// Store a password using the credential mode configured on disk.
///
/// # Errors
/// See [`CredentialStore::set_password`].
pub async fn set_password(server_name: &str, username: &str, password: &str) -> crate::Result<()> {
    let cfg = Config::load()?;
    CredentialStore::from_config(&cfg)?
        .set_password(server_name, username, password)
        .await
}

/// Remove a stored password using the credential mode configured on disk.
///
/// # Errors
/// See [`CredentialStore::delete_password`].
pub async fn delete_password(server_name: &str, username: &str) -> crate::Result<()> {
    let cfg = Config::load()?;
    CredentialStore::from_config(&cfg)?
        .delete_password(server_name, username)
        .await
}

// -----------------------------------------------------------------------------
// Prompt implementation
// -----------------------------------------------------------------------------

async fn prompt_for_password(server: &str, user: &str) -> crate::Result<String> {
    let msg = format!("Password for {user}@{server}: ");
    tokio::task::spawn_blocking(move || rpassword::prompt_password(&msg).map_err(RustmoteError::Io))
        .await
        .map_err(|e| io_from_join(&e))?
}

// -----------------------------------------------------------------------------
// Unsafe-mode file helpers
// -----------------------------------------------------------------------------

async fn read_unsafe_file(path: &Path) -> crate::Result<UnsafeFile> {
    check_unsafe_permissions(path)?;
    match tokio::fs::read_to_string(path).await {
        Ok(raw) => toml::from_str(&raw).map_err(|e| RustmoteError::ConfigParse {
            path: path.to_path_buf(),
            source: e,
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(UnsafeFile::default()),
        Err(e) => Err(RustmoteError::Io(e)),
    }
}

async fn write_unsafe_file(path: &Path, file: &UnsafeFile) -> crate::Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (path, file);
        return Err(RustmoteError::UnsafeModeUnsupportedOnPlatform);
    }
    #[cfg(unix)]
    {
        use tokio::io::AsyncWriteExt;

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let serialized = toml::to_string_pretty(file)?;
        let mut f = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .await?;
        f.write_all(serialized.as_bytes()).await?;
        f.sync_all().await?;
        Ok(())
    }
}

/// Verify that the credentials file has owner-only `0600` permissions on
/// Unix. Returns `Ok(())` if the file does not yet exist.
///
/// On non-Unix platforms unsafe mode is currently refused — callers should
/// surface the resulting [`RustmoteError::UnsafeModeUnsupportedOnPlatform`]
/// with a hint to use keychain mode instead.
pub fn check_unsafe_permissions(path: &Path) -> crate::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(meta) => {
                let mode = meta.permissions().mode() & 0o777;
                if mode == 0o600 {
                    Ok(())
                } else {
                    Err(RustmoteError::InsecureCredentialsFile(mode))
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(RustmoteError::Io(e)),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(RustmoteError::UnsafeModeUnsupportedOnPlatform)
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

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

    #[test]
    fn keyring_account_format_matches_spec() {
        assert_eq!(
            keyring_account("zima-brain", "charles"),
            "zima-brain:charles"
        );
    }

    #[test]
    fn account_split_roundtrip() {
        let acct = keyring_account("zima-brain", "charles");
        assert_eq!(split_account_server(&acct), "zima-brain");
        assert_eq!(split_account_user(&acct), "charles");
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_permissions_accepts_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "rustmote-perm-ok-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.toml");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_unsafe_permissions(&path).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_permissions_refuses_0644() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "rustmote-perm-bad-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.toml");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        match check_unsafe_permissions(&path).unwrap_err() {
            RustmoteError::InsecureCredentialsFile(mode) => assert_eq!(mode, 0o644),
            other => panic!("unexpected error: {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_permissions_missing_file_is_ok() {
        let path = std::env::temp_dir().join(format!(
            "rustmote-perm-missing-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        assert!(!path.exists());
        assert!(check_unsafe_permissions(&path).is_ok());
    }

    #[tokio::test]
    async fn prompt_set_and_delete_are_noops() {
        let store = CredentialStore::prompt();
        store.set_password("s", "u", "pw").await.unwrap();
        store.delete_password("s", "u").await.unwrap();
    }
}
