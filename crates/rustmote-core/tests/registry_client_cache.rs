//! Integration test for `registry_client`'s TTL cache per spec §7.
//!
//! Hits a counting in-process mock transport — the real registry is
//! never contacted, so this suite runs hermetically in CI without
//! Docker Hub rate-limit concerns.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rustmote_core::registry_client::{CacheStore, RegistryClient, RegistryTransport, DEFAULT_TTL};

struct ScriptedTransport {
    tag_calls: Arc<AtomicUsize>,
    digest_calls: Arc<AtomicUsize>,
    tags: Vec<String>,
    digest: String,
}

#[async_trait]
impl RegistryTransport for ScriptedTransport {
    async fn list_tags(&self, _repo: &str) -> rustmote_core::Result<Vec<String>> {
        self.tag_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.tags.clone())
    }
    async fn resolve_digest(&self, _repo: &str, _tag: &str) -> rustmote_core::Result<String> {
        self.digest_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.digest.clone())
    }
}

fn scripted(
    tags: Vec<String>,
    digest: String,
) -> (ScriptedTransport, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let tag_calls = Arc::new(AtomicUsize::new(0));
    let digest_calls = Arc::new(AtomicUsize::new(0));
    (
        ScriptedTransport {
            tag_calls: Arc::clone(&tag_calls),
            digest_calls: Arc::clone(&digest_calls),
            tags,
            digest,
        },
        tag_calls,
        digest_calls,
    )
}

/// Transport that panics on contact — used to prove the cache, not the
/// transport, served a call.
struct PanicOnContact;

#[async_trait]
impl RegistryTransport for PanicOnContact {
    async fn list_tags(&self, _: &str) -> rustmote_core::Result<Vec<String>> {
        panic!("transport must not be contacted while cache entry is fresh");
    }
    async fn resolve_digest(&self, _: &str, _: &str) -> rustmote_core::Result<String> {
        unreachable!()
    }
}

#[tokio::test]
async fn second_call_within_ttl_reads_disk_cache_without_transport_call() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("docker-hub-cache.toml");

    let (t, tag_calls, _) = scripted(vec!["1.1.11".into(), "latest".into()], "sha256:x".into());
    let client = RegistryClient::with_transport(Box::new(t))
        .with_cache_path(cache.clone())
        .with_ttl(DEFAULT_TTL);

    let first = client.list_tags("rustdesk/rustdesk-server").await.unwrap();
    assert_eq!(first, vec!["1.1.11", "latest"]);
    assert_eq!(tag_calls.load(Ordering::SeqCst), 1);
    assert!(cache.is_file(), "cache file should be written after miss");

    // Build a **new** client pointed at the same cache path with a
    // transport that would fail if contacted — proving that the on-disk
    // cache, not just the in-memory one, is what serves the second call.
    let fresh_client = RegistryClient::with_transport(Box::new(PanicOnContact))
        .with_cache_path(cache.clone())
        .with_ttl(DEFAULT_TTL);
    let second = fresh_client
        .list_tags("rustdesk/rustdesk-server")
        .await
        .unwrap();
    assert_eq!(second, vec!["1.1.11", "latest"]);
}

#[tokio::test]
async fn expired_entry_triggers_a_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("docker-hub-cache.toml");

    // Seed the on-disk cache with an entry whose `cached_at` is 2 hours
    // ago — older than the default 1h TTL.
    let mut seed = CacheStore::default();
    let stale_at = chrono::Utc::now() - chrono::Duration::hours(2);
    seed.put_tag_list("rustdesk/rustdesk-server", vec!["old".into()], stale_at);
    seed.save_to(&cache_path).unwrap();

    let (t, tag_calls, _) = scripted(vec!["1.1.11".into()], "sha256:y".into());
    let client = RegistryClient::with_transport(Box::new(t))
        .with_cache_path(cache_path.clone())
        .with_ttl(DEFAULT_TTL);

    let tags = client.list_tags("rustdesk/rustdesk-server").await.unwrap();
    assert_eq!(
        tag_calls.load(Ordering::SeqCst),
        1,
        "stale entry must not short-circuit the transport call",
    );
    assert_eq!(
        tags,
        vec!["1.1.11"],
        "expect the refreshed tags, not the stale ones"
    );

    // Persisted cache now reflects the refreshed list.
    let reloaded = CacheStore::load_from(&cache_path).unwrap();
    let entry = reloaded
        .tags
        .get("rustdesk/rustdesk-server")
        .expect("cache should have been rewritten");
    assert_eq!(entry.tags, vec!["1.1.11"]);
}

#[tokio::test]
async fn digest_and_tag_list_caches_do_not_collide() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("docker-hub-cache.toml");

    let (t, tag_calls, digest_calls) = scripted(vec!["1.1.11".into()], "sha256:zzz".into());
    let client = RegistryClient::with_transport(Box::new(t))
        .with_cache_path(cache)
        .with_ttl(Duration::from_secs(3600));

    client.list_tags("rustdesk/rustdesk-server").await.unwrap();
    client
        .resolve_digest("rustdesk/rustdesk-server", "1.1.11")
        .await
        .unwrap();
    client.list_tags("rustdesk/rustdesk-server").await.unwrap();
    client
        .resolve_digest("rustdesk/rustdesk-server", "1.1.11")
        .await
        .unwrap();

    assert_eq!(
        tag_calls.load(Ordering::SeqCst),
        1,
        "tags/list should have been cached after the first call",
    );
    assert_eq!(
        digest_calls.load(Ordering::SeqCst),
        1,
        "manifest HEAD should have been cached after the first call",
    );
}
