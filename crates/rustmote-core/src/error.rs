//! Error types for `rustmote-core`.
//!
//! The `RustmoteError` enum enumerates every failure mode the library can
//! return. Per spec §3.9, the library uses `thiserror`; the CLI wraps these
//! in `anyhow::Context` for top-level error reporting.
//!
//! Spec §3.9 pins the initial variant set. Additional variants may be added
//! in future phases; existing ones are not removed without a DECISION record.

use std::path::PathBuf;

/// Fallible result type used throughout the library.
pub type Result<T> = std::result::Result<T, RustmoteError>;

/// Errors produced by the Rustmote core library.
#[derive(thiserror::Error, Debug)]
pub enum RustmoteError {
    #[error("config file not found at {0}")]
    ConfigNotFound(PathBuf),

    #[error("server '{0}' not in registry")]
    UnknownServer(String),

    #[error("server '{0}' is already registered")]
    ServerAlreadyExists(String),

    #[error("target '{0}' not in registry")]
    UnknownTarget(String),

    #[error("target '{0}' is already registered")]
    TargetAlreadyExists(String),

    #[error("could not resolve a config directory for the current platform")]
    NoConfigDir,

    #[error("failed to parse config file {path}: {source}")]
    ConfigParse {
        path: std::path::PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to serialize config: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),

    #[error("ssh connection failed: {0}")]
    SshConnection(#[from] russh::Error),

    #[error("credential access failed: {0}")]
    Credential(#[from] keyring::Error),

    #[error("viewer binary not found; install RustDesk or set viewer_path")]
    ViewerNotFound,

    #[error("unsafe mode requires explicit acknowledgment flag")]
    UnsafeModeNotAcknowledged,

    #[error("credentials file has insecure permissions: {0:o} (expected 600)")]
    InsecureCredentialsFile(u32),

    #[error(
        "unsafe credential mode is not supported on this platform yet; use `keychain` mode instead"
    )]
    UnsafeModeUnsupportedOnPlatform,

    #[error("no stored credential for {user}@{server}")]
    NoStoredCredential { server: String, user: String },

    #[error("docker hub api error: {0}")]
    RegistryApi(String),

    #[error("relay not installed on server '{0}'; run `rustmote relay bootstrap` first")]
    RelayNotInstalled(String),

    #[error("relay already installed at {0}; remove it or use a different path")]
    RelayAlreadyInstalled(PathBuf),

    #[error("relay health check failed after update; rolled back to previous version")]
    RelayHealthCheckFailed,

    #[error("docker compose v1 detected; rustmote requires v2 (the `docker compose` plugin)")]
    DockerComposeV1Detected,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
