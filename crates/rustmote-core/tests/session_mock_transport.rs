//! Integration test for spec §7.1: the `RemoteExec` trait must be the
//! seam relay-lifecycle tests mock. This test verifies the trait is
//! object-safe, that a mock implementation can stand in for the real
//! `Session`, and that the `ExecOutput::ok_for` helper surfaces
//! non-zero exits as `RustmoteError::RemoteCommandFailed`.
//!
//! The real russh-backed `Session::open` path requires a live SSH
//! server and is therefore only exercised by opt-in tests gated on
//! `RUSTMOTE_INTEGRATION_SSH=1` — not run in CI.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use rustmote_core::error::RustmoteError;
use rustmote_core::session::{ExecOutput, RemoteExec};

// -----------------------------------------------------------------------------
// MockTransport — records issued commands and answers from a scripted map.
// -----------------------------------------------------------------------------

#[derive(Default)]
struct MockTransport {
    scripted: Mutex<HashMap<String, ExecOutput>>,
    history: Mutex<Vec<String>>,
}

impl MockTransport {
    fn script(&self, cmd: &str, out: ExecOutput) {
        self.scripted.lock().unwrap().insert(cmd.to_owned(), out);
    }

    fn history(&self) -> Vec<String> {
        self.history.lock().unwrap().clone()
    }
}

#[async_trait]
impl RemoteExec for MockTransport {
    async fn exec(&self, command: &str) -> rustmote_core::Result<ExecOutput> {
        self.history.lock().unwrap().push(command.to_owned());
        self.scripted
            .lock()
            .unwrap()
            .get(command)
            .cloned()
            .ok_or_else(|| RustmoteError::RemoteCommandFailed {
                command: command.to_owned(),
                exit_code: 127,
                stderr: "no scripted response in MockTransport".into(),
            })
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[tokio::test]
async fn mock_transport_returns_scripted_output() {
    let t = MockTransport::default();
    t.script(
        "docker compose version",
        ExecOutput {
            stdout: b"Docker Compose version v2.24.0\n".to_vec(),
            stderr: vec![],
            exit_code: 0,
        },
    );

    let out = t.exec("docker compose version").await.unwrap();
    assert_eq!(out.exit_code, 0);
    assert!(out.stdout_string().contains("v2.24.0"));
    assert_eq!(t.history(), vec!["docker compose version".to_owned()]);
}

#[tokio::test]
async fn mock_transport_can_be_used_behind_trait_object() {
    let concrete = MockTransport::default();
    concrete.script(
        "whoami",
        ExecOutput {
            stdout: b"charles\n".to_vec(),
            stderr: vec![],
            exit_code: 0,
        },
    );
    // This is the shape relay_lifecycle will use — a `Box<dyn RemoteExec>`
    // so the state machines can be tested against a mock.
    let transport: Box<dyn RemoteExec> = Box::new(concrete);
    let out = transport.exec("whoami").await.unwrap();
    assert_eq!(out.stdout_string().trim(), "charles");
}

#[tokio::test]
async fn ok_for_propagates_nonzero_exit_as_error() {
    let t = MockTransport::default();
    t.script(
        "ls /nope",
        ExecOutput {
            stdout: vec![],
            stderr: b"ls: cannot access '/nope': No such file or directory\n".to_vec(),
            exit_code: 2,
        },
    );

    let out = t.exec("ls /nope").await.unwrap();
    match out.ok_for("ls /nope").unwrap_err() {
        RustmoteError::RemoteCommandFailed {
            command,
            exit_code,
            stderr,
        } => {
            assert_eq!(command, "ls /nope");
            assert_eq!(exit_code, 2);
            assert!(stderr.contains("No such file or directory"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn ok_for_passes_zero_exit() {
    let t = MockTransport::default();
    t.script(
        "true",
        ExecOutput {
            stdout: vec![],
            stderr: vec![],
            exit_code: 0,
        },
    );
    let out = t.exec("true").await.unwrap();
    out.ok_for("true").unwrap();
}
