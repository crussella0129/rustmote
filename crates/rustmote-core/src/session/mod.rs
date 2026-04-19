//! `russh`-based SSH session, host-key TOFU, and local port forwarding.
//!
//! Implements spec §3.4 (session orchestration) and §6.7 (mandatory
//! host-key TOFU verification). The flow per connection is:
//!
//! 1. Connect to `server.host:server.ssh_port` with a [`HostKeyHandler`]
//!    that defers to [`known_hosts::KnownHosts`].
//! 2. Try key-based auth in order: config override → `~/.ssh/id_ed25519`
//!    → `~/.ssh/id_rsa`. Fall back to password auth via
//!    [`crate::credentials`] if no key succeeds.
//! 3. Bind a random free local port on `127.0.0.1` and forward incoming
//!    connections to `127.0.0.1:server.relay_port` on the remote via
//!    SSH `direct-tcpip` channels.
//! 4. Return a [`Session`] handle that owns the forwarder task and
//!    tears it down on drop.
//!
//! The [`RemoteExec`] trait is the seam relay-lifecycle tests mock
//! (spec §7.1: "mock the SSH layer with a trait abstraction").

pub mod known_hosts;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use russh::client::{self, Handle, Handler};
use russh::keys::key::{KeyPair, PublicKey};
use russh::keys::load_secret_key;
use russh::{ChannelMsg, Disconnect};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::error::RustmoteError;
use crate::registry::RemoteServer;

pub use known_hosts::{HostKey, KnownHosts, TofuOutcome, TofuPolicy, KNOWN_HOSTS_FILE_NAME};

/// How authentication will be attempted. Constructed by the CLI once
/// per connection; passed to [`Session::open`] by value so the
/// password material never leaks into process argv.
#[derive(Debug, Clone, Default)]
pub struct AuthMaterial {
    /// Explicit private-key paths to try before the default locations.
    /// Passed via `--key-path` or a per-server config override.
    pub extra_key_paths: Vec<PathBuf>,

    /// Passphrase for any encrypted key encountered. Applied to every
    /// candidate — if different keys need different passphrases, call
    /// with each distinct pair.
    pub key_passphrase: Option<String>,

    /// Password for password-auth fallback. Resolved by the caller from
    /// [`crate::credentials::CredentialStore`]; `None` disables password
    /// auth.
    pub password: Option<String>,
}

/// Output of a remote command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: u32,
}

impl ExecOutput {
    /// Decode stdout as UTF-8, replacing invalid sequences.
    #[must_use]
    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Decode stderr as UTF-8, replacing invalid sequences.
    #[must_use]
    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// Return `self` if exit status is 0, else a
    /// [`RustmoteError::RemoteCommandFailed`] carrying the stderr.
    ///
    /// # Errors
    /// Non-zero exit codes are converted to an error. The command
    /// string is included for context.
    pub fn ok_for(self, command: &str) -> crate::Result<Self> {
        if self.exit_code == 0 {
            Ok(self)
        } else {
            Err(RustmoteError::RemoteCommandFailed {
                command: command.to_owned(),
                exit_code: self.exit_code,
                stderr: self.stderr_string(),
            })
        }
    }
}

/// Minimal remote-exec abstraction used by the relay-lifecycle state
/// machines. Real code gets an `impl RemoteExec for Session`; tests
/// substitute an in-memory stub.
///
/// `#[async_trait]` is used (rather than the native 1.75 async-in-trait
/// feature) so the trait is object-safe — `relay_lifecycle` holds a
/// `Box<dyn RemoteExec>` to swap transports at runtime.
#[async_trait]
pub trait RemoteExec: Send + Sync {
    /// Run `command` on the remote and collect its output. The command
    /// string is passed verbatim to the remote shell — callers are
    /// responsible for validating against an allowlist per spec §6.4.
    ///
    /// # Errors
    /// Propagates the transport's native error; does **not** auto-fail
    /// on non-zero exit status — use [`ExecOutput::ok_for`] for that.
    async fn exec(&self, command: &str) -> crate::Result<ExecOutput>;
}

// -----------------------------------------------------------------------------
// russh Handler: host-key TOFU decision point
// -----------------------------------------------------------------------------

/// Handler that captures the server's public key during the SSH
/// handshake and consults [`KnownHosts`].
///
/// The `decision` is fed back out via a shared slot so the caller can
/// persist [`TofuOutcome::Pinned`] after the handshake completes.
struct HostKeyHandler {
    host: String,
    port: u16,
    known_hosts: Arc<Mutex<KnownHosts>>,
    policy: TofuPolicy,
    outcome_slot: Arc<Mutex<Option<TofuOutcome>>>,
}

#[async_trait]
impl Handler for HostKeyHandler {
    type Error = RustmoteError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        let observed = HostKey {
            fingerprint: format!("SHA256:{}", server_public_key.fingerprint()),
            key_type: server_public_key.name().to_owned(),
            first_seen: chrono::Utc::now(),
        };
        let outcome = {
            let mut guard = self.known_hosts.lock().unwrap();
            guard.verify_or_pin(&self.host, self.port, &observed, self.policy)
        };
        // Record the outcome before returning — the caller reads this
        // slot after `client::connect` returns to decide whether to
        // persist a newly-pinned entry or surface a mismatch.
        *self.outcome_slot.lock().unwrap() = Some(outcome.clone());
        match outcome {
            TofuOutcome::Matched | TofuOutcome::Pinned => Ok(true),
            TofuOutcome::Mismatch {
                expected,
                actual_fingerprint,
            } => Err(RustmoteError::HostKeyMismatch {
                host: self.host.clone(),
                port: self.port,
                expected: expected.fingerprint,
                actual: actual_fingerprint,
            }),
            TofuOutcome::UnknownRejected => Err(RustmoteError::HostKeyUnknown {
                host: self.host.clone(),
                port: self.port,
            }),
        }
    }
}

// -----------------------------------------------------------------------------
// Session
// -----------------------------------------------------------------------------

/// Live SSH session with an outbound port forward.
///
/// Holds the russh client handle and the forwarder task. Dropping the
/// session disconnects the SSH connection and cancels the forwarder.
pub struct Session {
    handle: Arc<Handle<HostKeyHandler>>,
    local_port: u16,
    server_name: String,
    // Kept so the forwarder task is aborted on drop.
    forwarder: Option<ForwarderHandle>,
}

struct ForwarderHandle {
    task: JoinHandle<()>,
    shutdown: oneshot::Sender<()>,
}

impl Session {
    /// Open a session against `server`, perform host-key TOFU against
    /// `known_hosts`, authenticate, and stand up a local port forward
    /// to the relay.
    ///
    /// `known_hosts` is taken by `Arc<Mutex<..>>` so the caller can
    /// observe TOFU updates and persist after `open` returns. The
    /// returned [`Session::was_newly_pinned`] flag tells the caller
    /// whether a new entry was added.
    ///
    /// # Errors
    /// - [`RustmoteError::HostKeyMismatch`] on fingerprint drift
    /// - [`RustmoteError::HostKeyUnknown`] under [`TofuPolicy::Strict`]
    /// - [`RustmoteError::SshAuthFailed`] when no auth method succeeds
    /// - [`RustmoteError::SshConnection`] for protocol/transport errors
    pub async fn open(
        server: &RemoteServer,
        auth: AuthMaterial,
        known_hosts: Arc<Mutex<KnownHosts>>,
        policy: TofuPolicy,
    ) -> crate::Result<(Self, bool)> {
        let host = server.host.to_string();
        let port = server.ssh_port;
        let outcome_slot = Arc::new(Mutex::new(None));

        let handler = HostKeyHandler {
            host: host.clone(),
            port,
            known_hosts,
            policy,
            outcome_slot: outcome_slot.clone(),
        };

        let config = Arc::new(client::Config::default());
        let mut handle = client::connect(config, (host.as_str(), port), handler).await?;

        authenticate(&mut handle, server, &auth).await?;

        let was_newly_pinned = matches!(*outcome_slot.lock().unwrap(), Some(TofuOutcome::Pinned));

        let handle = Arc::new(handle);
        let (local_port, forwarder) = spawn_forwarder(handle.clone(), server.relay_port).await?;

        Ok((
            Self {
                handle,
                local_port,
                server_name: server.name.clone(),
                forwarder: Some(forwarder),
            },
            was_newly_pinned,
        ))
    }

    /// Local TCP port the forwarder is listening on.
    #[must_use]
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Name of the `RemoteServer` this session is attached to.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Explicitly tear down the session. Equivalent to dropping, but
    /// returns any error from the disconnect round-trip.
    ///
    /// # Errors
    /// Propagates `russh::Error` from the disconnect message.
    pub async fn close(mut self) -> crate::Result<()> {
        if let Some(fwd) = self.forwarder.take() {
            let _ = fwd.shutdown.send(());
            fwd.task.abort();
        }
        self.handle
            .disconnect(Disconnect::ByApplication, "rustmote session closing", "en")
            .await?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(fwd) = self.forwarder.take() {
            let _ = fwd.shutdown.send(());
            fwd.task.abort();
        }
    }
}

#[async_trait]
impl RemoteExec for Session {
    async fn exec(&self, command: &str) -> crate::Result<ExecOutput> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code: u32 = 0;

        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext: 1 } => {
                    stderr.extend_from_slice(&data);
                }
                ChannelMsg::ExitStatus { exit_status } => exit_code = exit_status,
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }

        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code,
        })
    }
}

// -----------------------------------------------------------------------------
// Authentication
// -----------------------------------------------------------------------------

async fn authenticate(
    handle: &mut Handle<HostKeyHandler>,
    server: &RemoteServer,
    auth: &AuthMaterial,
) -> crate::Result<()> {
    let mut tried: Vec<&str> = Vec::new();
    let mut last_error = String::new();

    // 1. Key-based auth — explicit overrides first, then defaults.
    let candidates = default_key_paths(&auth.extra_key_paths);
    for path in &candidates {
        if !path.exists() {
            continue;
        }
        tried.push("publickey");
        match load_secret_key(path, auth.key_passphrase.as_deref()) {
            Ok(key) => {
                let key_arc = Arc::new(key);
                match handle
                    .authenticate_publickey(&server.ssh_user, key_arc)
                    .await
                {
                    Ok(true) => {
                        tracing::info!(
                            user = %server.ssh_user,
                            host = %server.host,
                            key = %path.display(),
                            "ssh authenticated via publickey"
                        );
                        return Ok(());
                    }
                    Ok(false) => {
                        last_error = format!("server rejected key {}", path.display());
                    }
                    Err(e) => {
                        last_error = format!("publickey auth error with {}: {e}", path.display());
                    }
                }
            }
            Err(e) => {
                last_error = format!("could not load {}: {e}", path.display());
            }
        }
    }

    // 2. Password fallback.
    if let Some(pw) = &auth.password {
        tried.push("password");
        match handle
            .authenticate_password(&server.ssh_user, pw.clone())
            .await
        {
            Ok(true) => {
                tracing::info!(
                    user = %server.ssh_user,
                    host = %server.host,
                    "ssh authenticated via password"
                );
                return Ok(());
            }
            Ok(false) => {
                last_error = "server rejected password".into();
            }
            Err(e) => {
                last_error = format!("password auth error: {e}");
            }
        }
    }

    Err(RustmoteError::SshAuthFailed {
        user: server.ssh_user.clone(),
        host: server.host.to_string(),
        methods: if tried.is_empty() {
            "(none — no key found, no password supplied)".into()
        } else {
            tried.join(",")
        },
        last_error,
    })
}

/// Build the ordered key-path candidate list: explicit overrides first,
/// then the spec §3.4 defaults (`~/.ssh/id_ed25519`, `~/.ssh/id_rsa`).
fn default_key_paths(extras: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = extras.to_vec();
    if let Some(home) = home_dir() {
        out.push(home.join(".ssh").join("id_ed25519"));
        out.push(home.join(".ssh").join("id_rsa"));
    }
    out
}

fn home_dir() -> Option<PathBuf> {
    #[allow(deprecated)]
    std::env::home_dir()
}

/// Programmatic hint used by tests that want to exercise
/// [`load_secret_key`] without going through a real SSH handshake.
///
/// # Errors
/// Propagates any error from `russh_keys::load_secret_key` as
/// [`RustmoteError::SshConnection`] (wraps `russh::Error`).
pub fn load_ssh_key(path: &Path, passphrase: Option<&str>) -> crate::Result<KeyPair> {
    load_secret_key(path, passphrase).map_err(|e| {
        RustmoteError::SshConnection(russh::Error::IO(std::io::Error::other(format!(
            "load key {}: {e}",
            path.display()
        ))))
    })
}

// -----------------------------------------------------------------------------
// Port forwarding
// -----------------------------------------------------------------------------

/// Bind a random free local port on 127.0.0.1 and spawn a task that
/// proxies each accepted connection to `127.0.0.1:remote_port` via
/// `direct-tcpip`.
async fn spawn_forwarder(
    handle: Arc<Handle<HostKeyHandler>>,
    remote_port: u16,
) -> crate::Result<(u16, ForwarderHandle)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_port = listener.local_addr()?.port();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let task = tokio::spawn(async move {
        forwarder_loop(listener, handle, remote_port, shutdown_rx).await;
    });

    Ok((
        local_port,
        ForwarderHandle {
            task,
            shutdown: shutdown_tx,
        },
    ))
}

async fn forwarder_loop(
    listener: TcpListener,
    handle: Arc<Handle<HostKeyHandler>>,
    remote_port: u16,
    mut shutdown: oneshot::Receiver<()>,
) {
    let (err_tx, mut err_rx) = mpsc::unbounded_channel::<RustmoteError>();

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(e) = err_rx.recv() => {
                tracing::warn!(error = %e, "port forward proxy error");
            }
            accept = listener.accept() => {
                let (local, peer) = match accept {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "local accept failed; forwarder exiting");
                        break;
                    }
                };
                let handle = Arc::clone(&handle);
                let err_tx = err_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = proxy_one(handle, local, peer, remote_port).await {
                        let _ = err_tx.send(e);
                    }
                });
            }
        }
    }
}

async fn proxy_one(
    handle: Arc<Handle<HostKeyHandler>>,
    local: TcpStream,
    peer: SocketAddr,
    remote_port: u16,
) -> crate::Result<()> {
    let channel = handle
        .channel_open_direct_tcpip(
            "127.0.0.1",
            u32::from(remote_port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await?;

    let channel_stream = channel.into_stream();
    let (mut cr, mut cw) = tokio::io::split(channel_stream);
    let (mut lr, mut lw) = local.into_split();

    // local -> channel
    let up = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = lr.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            cw.write_all(&buf[..n]).await?;
        }
        cw.shutdown().await.ok();
        Ok::<_, std::io::Error>(())
    };

    // channel -> local
    let down = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = cr.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            lw.write_all(&buf[..n]).await?;
        }
        lw.shutdown().await.ok();
        Ok::<_, std::io::Error>(())
    };

    // Proxy both directions; the first half-close tears down the pair.
    tokio::try_join!(up, down).map_err(RustmoteError::Io)?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_output_stderr_and_stdout_decode() {
        let o = ExecOutput {
            stdout: b"hello\n".to_vec(),
            stderr: b"err\n".to_vec(),
            exit_code: 0,
        };
        assert_eq!(o.stdout_string(), "hello\n");
        assert_eq!(o.stderr_string(), "err\n");
    }

    #[test]
    fn exec_output_ok_for_zero_passes() {
        let o = ExecOutput {
            stdout: vec![],
            stderr: vec![],
            exit_code: 0,
        };
        o.ok_for("true").unwrap();
    }

    #[test]
    fn exec_output_ok_for_nonzero_errors() {
        let o = ExecOutput {
            stdout: vec![],
            stderr: b"boom".to_vec(),
            exit_code: 2,
        };
        match o.ok_for("false").unwrap_err() {
            RustmoteError::RemoteCommandFailed {
                command,
                exit_code,
                stderr,
            } => {
                assert_eq!(command, "false");
                assert_eq!(exit_code, 2);
                assert_eq!(stderr, "boom");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn default_key_paths_respects_extras_first() {
        let extra = PathBuf::from("/tmp/my-key");
        let paths = default_key_paths(std::slice::from_ref(&extra));
        assert_eq!(paths.first(), Some(&extra));
    }

    #[test]
    fn default_key_paths_appends_standard_locations() {
        let paths = default_key_paths(&[]);
        // At least id_ed25519 and id_rsa should be present when $HOME is set.
        if home_dir().is_some() {
            assert!(paths.iter().any(|p| p.ends_with("id_ed25519")));
            assert!(paths.iter().any(|p| p.ends_with("id_rsa")));
        }
    }
}
