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

    #[error(
        "host key mismatch for {host}:{port} — pinned {expected}, observed {actual}; \
         refusing to connect (spec §6.7). Remove the stale entry from known_hosts.toml \
         only after verifying the new key out-of-band."
    )]
    HostKeyMismatch {
        host: String,
        port: u16,
        expected: String,
        actual: String,
    },

    #[error("host key for {host}:{port} is not pinned and strict TOFU policy refuses first-use")]
    HostKeyUnknown { host: String, port: u16 },

    #[error(
        "ssh authentication failed for {user}@{host}; tried {methods} (last error: {last_error})"
    )]
    SshAuthFailed {
        user: String,
        host: String,
        methods: String,
        last_error: String,
    },

    #[error("no SSH private key found at any of: {0}")]
    NoSshKeyFound(String),

    #[error("remote command '{command}' exited with status {exit_code}: {stderr}")]
    RemoteCommandFailed {
        command: String,
        exit_code: u32,
        stderr: String,
    },

    #[error("credential access failed: {0}")]
    Credential(#[from] keyring::Error),

    #[error("viewer binary not found; install RustDesk or set viewer_path")]
    ViewerNotFound,

    #[error("invalid RustDesk target id '{0}'; expected 9 or 10 ASCII digits (spec §3.5)")]
    InvalidTargetId(String),

    #[error("invalid server name '{0}'; expected 1-64 chars matching [a-zA-Z0-9_-] (spec §6.4)")]
    InvalidServerName(String),

    #[error("no non-loopback IPv4 interface with a CIDR prefix found for discovery auto-detect")]
    DiscoveryNoInterface,

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

    #[error(
        "docker engine not installed on the remote; re-run with --install-docker to let \
         rustmote run the official install script (spec §6.11)"
    )]
    DockerEngineNotInstalled,

    #[error(
        "unsupported relay OS: {0}. Rustmote can bootstrap Debian, Ubuntu, and Arch \
         automatically; install docker manually and re-run on anything else"
    )]
    RelayUnsupportedOs(String),

    #[error(
        "relay install path {0} exists but is not a rustmote install (no .rustmote-state.toml); \
         refusing to overwrite (spec §6.12)"
    )]
    RelayForeignInstall(PathBuf),

    #[error(
        "refusing to run relay update in non-interactive mode without --yes (spec §5.1.3 step 4)"
    )]
    RelayUpdateNotConfirmed,

    #[error("invalid relay remote path or argument: '{0}'")]
    InvalidRelayPath(String),

    #[error("invalid docker image reference: '{0}'")]
    InvalidImageRef(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
