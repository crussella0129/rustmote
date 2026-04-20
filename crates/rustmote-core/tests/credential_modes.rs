//! Integration test for spec §7.2: exercise all three credential modes.
//!
//! - `Prompt` — covered by a no-op smoke (the actual terminal prompt path is
//!   not exercised here; spec §3.3 specifies it as interactive-only).
//! - `Keychain` — via a `MockKeyring` (in-memory `HashMap`) injected through
//!   the `KeyringBackend` trait seam.
//! - `Unsafe` — round-tripped against a scratch `credentials.toml` with
//!   `0600` permissions on Unix.

use std::collections::HashMap;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rustmote_core::credentials::{CredentialStore, KeyringBackend, KEYRING_SERVICE};
use rustmote_core::error::RustmoteError;

// -----------------------------------------------------------------------------
// Mock keyring
// -----------------------------------------------------------------------------

#[derive(Default)]
struct MockKeyring {
    inner: Mutex<HashMap<(String, String), String>>,
}

impl KeyringBackend for MockKeyring {
    fn get(&self, service: &str, account: &str) -> rustmote_core::Result<String> {
        let guard = self.inner.lock().unwrap();
        guard
            .get(&(service.to_owned(), account.to_owned()))
            .cloned()
            .ok_or_else(|| RustmoteError::NoStoredCredential {
                server: account
                    .split_once(':')
                    .map_or(account, |(s, _)| s)
                    .to_owned(),
                user: account.split_once(':').map_or("", |(_, u)| u).to_owned(),
            })
    }

    fn set(&self, service: &str, account: &str, password: &str) -> rustmote_core::Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.insert(
            (service.to_owned(), account.to_owned()),
            password.to_owned(),
        );
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> rustmote_core::Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Scratch-path helper
// -----------------------------------------------------------------------------

#[cfg(unix)]
fn scratch_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "rustmote-cred-{}-{}-{}",
        tag,
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    p
}

// -----------------------------------------------------------------------------
// Keychain mode
// -----------------------------------------------------------------------------

#[tokio::test]
async fn keychain_set_get_delete_roundtrip() {
    let mock = Arc::new(MockKeyring::default());
    let store = CredentialStore::with_shared_keyring(mock.clone());

    store
        .set_password("zima", "charles", "hunter2")
        .await
        .unwrap();
    let got = store.get_password("zima", "charles").await.unwrap();
    assert_eq!(got, "hunter2");

    // Verify it's reachable via the same account format.
    {
        let map = mock.inner.lock().unwrap();
        assert_eq!(
            map.get(&(KEYRING_SERVICE.to_owned(), "zima:charles".to_owned()))
                .map(String::as_str),
            Some("hunter2")
        );
    }

    store.delete_password("zima", "charles").await.unwrap();
    match store.get_password("zima", "charles").await.unwrap_err() {
        RustmoteError::NoStoredCredential { server, user } => {
            assert_eq!(server, "zima");
            assert_eq!(user, "charles");
        }
        other => panic!("expected NoStoredCredential, got {other:?}"),
    }
}

#[tokio::test]
async fn keychain_delete_missing_is_idempotent() {
    let store = CredentialStore::with_keyring(MockKeyring::default());
    store.delete_password("nope", "nobody").await.unwrap();
    store.delete_password("nope", "nobody").await.unwrap();
}

#[tokio::test]
async fn keychain_missing_entry_returns_no_stored_credential() {
    let store = CredentialStore::with_keyring(MockKeyring::default());
    match store.get_password("zima", "charles").await.unwrap_err() {
        RustmoteError::NoStoredCredential { server, user } => {
            assert_eq!(server, "zima");
            assert_eq!(user, "charles");
        }
        other => panic!("expected NoStoredCredential, got {other:?}"),
    }
}

// -----------------------------------------------------------------------------
// Unsafe mode
// -----------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn unsafe_set_get_delete_roundtrip() {
    let dir = scratch_path("unsafe-ok");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("credentials.toml");
    let store = CredentialStore::with_unsafe_file(&path);

    store
        .set_password("zima", "charles", "hunter2")
        .await
        .unwrap();
    let got = store.get_password("zima", "charles").await.unwrap();
    assert_eq!(got, "hunter2");

    // Multiple entries coexist.
    store.set_password("zima", "root", "rootpw").await.unwrap();
    assert_eq!(store.get_password("zima", "root").await.unwrap(), "rootpw");
    assert_eq!(
        store.get_password("zima", "charles").await.unwrap(),
        "hunter2"
    );

    store.delete_password("zima", "charles").await.unwrap();
    match store.get_password("zima", "charles").await.unwrap_err() {
        RustmoteError::NoStoredCredential { .. } => {}
        other => panic!("expected NoStoredCredential, got {other:?}"),
    }
    // Remaining entry untouched.
    assert_eq!(store.get_password("zima", "root").await.unwrap(), "rootpw");

    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn unsafe_file_is_written_with_mode_600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch_path("unsafe-mode");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("credentials.toml");
    let store = CredentialStore::with_unsafe_file(&path);

    store.set_password("zima", "charles", "pw").await.unwrap();

    let meta = std::fs::metadata(&path).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "spec §6.3 requires 0600");

    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn unsafe_refuses_world_readable_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch_path("unsafe-bad");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("credentials.toml");
    // Pre-create with insecure permissions.
    std::fs::write(&path, "").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let store = CredentialStore::with_unsafe_file(&path);
    match store.get_password("zima", "charles").await.unwrap_err() {
        RustmoteError::InsecureCredentialsFile(mode) => assert_eq!(mode, 0o644),
        other => panic!("expected InsecureCredentialsFile, got {other:?}"),
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn unsafe_missing_file_returns_no_stored_credential() {
    let dir = scratch_path("unsafe-empty");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("credentials.toml");
    // File does not exist yet.
    let store = CredentialStore::with_unsafe_file(&path);
    match store.get_password("zima", "charles").await.unwrap_err() {
        RustmoteError::NoStoredCredential { .. } => {}
        other => panic!("expected NoStoredCredential, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

// -----------------------------------------------------------------------------
// Prompt mode
// -----------------------------------------------------------------------------

#[tokio::test]
async fn prompt_mode_set_and_delete_are_noops() {
    let store = CredentialStore::prompt();
    // Per spec §3.3, prompt mode never persists — set/delete succeed as no-ops.
    store.set_password("zima", "charles", "pw").await.unwrap();
    store.delete_password("zima", "charles").await.unwrap();
}
