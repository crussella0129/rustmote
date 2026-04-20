//! Docker Hub v2 registry API client with TTL caching.
//!
//! Implements spec §3.7:
//!
//! - Anonymous access against `registry-1.docker.io` with per-repo Bearer
//!   tokens fetched from `auth.docker.io` (the extension seam for private
//!   registries is [`RegistryTransport`] — the v0.1 ship is
//!   anonymous-only but the trait object lets a future phase slot in an
//!   authenticated transport without touching [`RegistryClient`]).
//! - Responsibilities: list tags, resolve a tag → manifest digest, fetch
//!   the manifest for format verification.
//! - Rate-limit-aware: responses cached at
//!   `$CACHE/rustmote/docker-hub-cache.toml` (TOML on disk, `BTreeMap`
//!   in memory) with a configurable TTL; default is 1 hour.
//!
//! The module is split into three concerns:
//!
//! - [`CacheStore`] — pure on-disk + in-memory TTL store. No network, no
//!   globals; testable without a transport.
//! - [`RegistryTransport`] — async trait capturing the two operations we
//!   make against Docker Hub. The [`HttpTransport`] impl uses `reqwest`
//!   with `rustls-tls` (no OpenSSL anywhere in the stack).
//! - [`RegistryClient`] — the user-facing handle that wires a transport
//!   to a cache under a TTL budget.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::RustmoteError;

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Default cache TTL per spec §3.7.
pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);

/// File name for the on-disk Docker Hub cache, joined under the resolved
/// cache directory.
pub const CACHE_FILE_NAME: &str = "docker-hub-cache.toml";

/// Environment variable that overrides the OS-appropriate cache
/// directory. Used by integration tests and packaging scenarios where
/// the default `ProjectDirs` location is inappropriate.
pub const CACHE_DIR_ENV: &str = "RUSTMOTE_CACHE_DIR";

/// Docker Hub v2 registry base. Exposed for tests that stand up a local
/// HTTP stub.
pub const DEFAULT_REGISTRY_BASE: &str = "https://registry-1.docker.io";

/// Docker Hub auth endpoint base.
pub const DEFAULT_AUTH_BASE: &str = "https://auth.docker.io";

// -----------------------------------------------------------------------------
// Cache path resolution
// -----------------------------------------------------------------------------

/// Returns the rustmote cache directory, honouring the `RUSTMOTE_CACHE_DIR`
/// override.
pub fn cache_dir() -> crate::Result<PathBuf> {
    if let Some(overridden) = std::env::var_os(CACHE_DIR_ENV) {
        return Ok(PathBuf::from(overridden));
    }
    ProjectDirs::from("", "", "rustmote")
        .map(|p| p.cache_dir().to_path_buf())
        .ok_or(RustmoteError::NoConfigDir)
}

/// Returns the fully-qualified path to `docker-hub-cache.toml`.
pub fn cache_path() -> crate::Result<PathBuf> {
    Ok(cache_dir()?.join(CACHE_FILE_NAME))
}

// -----------------------------------------------------------------------------
// Cache store — pure, testable without HTTP
// -----------------------------------------------------------------------------

/// Cache entry for a tag listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagsEntry {
    pub cached_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

/// Cache entry for a resolved `repo:tag → digest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestEntry {
    pub cached_at: DateTime<Utc>,
    pub digest: String,
}

/// On-disk TOML cache. Keyed by `"{repo}"` for tag listings and
/// `"{repo}:{tag}"` for digests — both are colon-safe in TOML and match
/// the strings users would see in a `docker pull` invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStore {
    #[serde(default)]
    pub tags: BTreeMap<String, TagsEntry>,

    #[serde(default)]
    pub digests: BTreeMap<String, DigestEntry>,
}

impl CacheStore {
    /// Load from an explicit path. Missing file → empty store.
    pub fn load_from(path: &Path) -> crate::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str(&raw).map_err(|e| RustmoteError::ConfigParse {
                path: path.to_path_buf(),
                source: e,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(RustmoteError::Io(e)),
        }
    }

    /// Atomic save via temp-file + rename. Creates parent directories if
    /// necessary.
    pub fn save_to(&self, path: &Path) -> crate::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, serialized)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Return cached tags if present AND still within `ttl` as of `now`.
    #[must_use]
    pub fn tag_list_if_fresh(
        &self,
        repo: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<Vec<String>> {
        let entry = self.tags.get(repo)?;
        if is_fresh(entry.cached_at, ttl, now) {
            Some(entry.tags.clone())
        } else {
            None
        }
    }

    /// Return a cached digest if present AND still within `ttl` as of `now`.
    #[must_use]
    pub fn digest_if_fresh(
        &self,
        repo: &str,
        tag: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let entry = self.digests.get(&digest_key(repo, tag))?;
        if is_fresh(entry.cached_at, ttl, now) {
            Some(entry.digest.clone())
        } else {
            None
        }
    }

    pub fn put_tag_list(&mut self, repo: &str, tags: Vec<String>, at: DateTime<Utc>) {
        self.tags.insert(
            repo.to_owned(),
            TagsEntry {
                cached_at: at,
                tags,
            },
        );
    }

    pub fn put_digest(&mut self, repo: &str, tag: &str, digest: String, at: DateTime<Utc>) {
        self.digests.insert(
            digest_key(repo, tag),
            DigestEntry {
                cached_at: at,
                digest,
            },
        );
    }
}

fn digest_key(repo: &str, tag: &str) -> String {
    format!("{repo}:{tag}")
}

fn is_fresh(cached_at: DateTime<Utc>, ttl: Duration, now: DateTime<Utc>) -> bool {
    let Ok(ttl_chrono) = chrono::Duration::from_std(ttl) else {
        // TTL too large to represent — treat as always-fresh.
        return true;
    };
    now.signed_duration_since(cached_at) < ttl_chrono
}

// -----------------------------------------------------------------------------
// Transport abstraction
// -----------------------------------------------------------------------------

/// Async seam between [`RegistryClient`] and the underlying HTTP client.
///
/// Tests and the `registry_client_cache` integration test substitute a
/// counting fake so we can verify cache hit/miss behavior without going
/// to the real registry. Future auth support plugs in as an alternate
/// implementation without touching callers.
#[async_trait]
pub trait RegistryTransport: Send + Sync {
    /// GET `/v2/{repo}/tags/list`. Repo must be in the form
    /// `"namespace/name"`. Docker's "official" single-name repos
    /// (e.g. `nginx`) require the caller to pass `library/nginx`.
    async fn list_tags(&self, repo: &str) -> crate::Result<Vec<String>>;

    /// HEAD `/v2/{repo}/manifests/{tag}` returning the
    /// `Docker-Content-Digest` header value (including the `sha256:`
    /// prefix).
    async fn resolve_digest(&self, repo: &str, tag: &str) -> crate::Result<String>;
}

// -----------------------------------------------------------------------------
// HTTP transport (reqwest + rustls-tls)
// -----------------------------------------------------------------------------

/// Anonymous-only HTTP transport hitting Docker Hub directly.
pub struct HttpTransport {
    http: reqwest::Client,
    registry_base: String,
    auth_base: String,
}

impl HttpTransport {
    /// Construct a transport against the real Docker Hub.
    pub fn new() -> crate::Result<Self> {
        Self::with_endpoints(DEFAULT_REGISTRY_BASE, DEFAULT_AUTH_BASE)
    }

    /// Construct a transport against arbitrary endpoints (used by tests
    /// to point at a local HTTP stub).
    pub fn with_endpoints(registry_base: &str, auth_base: &str) -> crate::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("rustmote/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| RustmoteError::RegistryApi(format!("building http client: {e}")))?;
        Ok(Self {
            http,
            registry_base: registry_base.trim_end_matches('/').to_owned(),
            auth_base: auth_base.trim_end_matches('/').to_owned(),
        })
    }

    /// Fetch an anonymous Bearer token scoped to `repository:{repo}:pull`.
    async fn fetch_token(&self, repo: &str) -> crate::Result<String> {
        #[derive(Deserialize)]
        struct TokenResponse {
            token: String,
        }
        let url = format!(
            "{}/token?service=registry.docker.io&scope=repository:{}:pull",
            self.auth_base, repo,
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| RustmoteError::RegistryApi(format!("token fetch: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RustmoteError::RegistryApi(format!(
                "token fetch returned {status}"
            )));
        }
        let parsed: TokenResponse = resp
            .json()
            .await
            .map_err(|e| RustmoteError::RegistryApi(format!("decoding token body: {e}")))?;
        Ok(parsed.token)
    }
}

#[async_trait]
impl RegistryTransport for HttpTransport {
    async fn list_tags(&self, repo: &str) -> crate::Result<Vec<String>> {
        #[derive(Deserialize)]
        struct TagsResponse {
            #[serde(default)]
            tags: Vec<String>,
        }
        let token = self.fetch_token(repo).await?;
        let url = format!("{}/v2/{repo}/tags/list", self.registry_base);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| RustmoteError::RegistryApi(format!("tags/list: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RustmoteError::RegistryApi(format!(
                "tags/list {repo} returned {status}"
            )));
        }
        let parsed: TagsResponse = resp
            .json()
            .await
            .map_err(|e| RustmoteError::RegistryApi(format!("decoding tags body: {e}")))?;
        Ok(parsed.tags)
    }

    async fn resolve_digest(&self, repo: &str, tag: &str) -> crate::Result<String> {
        let token = self.fetch_token(repo).await?;
        let url = format!("{}/v2/{repo}/manifests/{tag}", self.registry_base);
        // Accept both the Docker v2 schema and the OCI manifest types.
        // Image indexes (multi-arch) also appear on real-world pulls.
        let accept = [
            "application/vnd.docker.distribution.manifest.v2+json",
            "application/vnd.docker.distribution.manifest.list.v2+json",
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.oci.image.index.v1+json",
        ]
        .join(", ");
        let resp = self
            .http
            .head(&url)
            .bearer_auth(&token)
            .header("Accept", accept)
            .send()
            .await
            .map_err(|e| RustmoteError::RegistryApi(format!("manifest HEAD: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RustmoteError::RegistryApi(format!(
                "manifest HEAD {repo}:{tag} returned {status}"
            )));
        }
        let digest = resp
            .headers()
            .get("Docker-Content-Digest")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                RustmoteError::RegistryApi(format!(
                    "manifest HEAD {repo}:{tag} missing Docker-Content-Digest header"
                ))
            })?;
        if !digest.starts_with("sha256:") {
            return Err(RustmoteError::RegistryApi(format!(
                "manifest digest '{digest}' is not sha256-prefixed"
            )));
        }
        Ok(digest.to_owned())
    }
}

// -----------------------------------------------------------------------------
// RegistryClient — transport + cache + TTL
// -----------------------------------------------------------------------------

/// Thin cache-aware facade over a [`RegistryTransport`].
///
/// The cache is loaded lazily on the first hit, held in memory, and
/// flushed back to disk after every successful transport call — the
/// write volume is tiny (two fields of a few hundred bytes per entry),
/// so the simple "load once, write-through after miss" policy avoids
/// the complexity of background flushes while still surviving crashes
/// between invocations.
pub struct RegistryClient {
    transport: Box<dyn RegistryTransport>,
    cache: Mutex<CacheStore>,
    cache_path: Option<PathBuf>,
    ttl: Duration,
}

impl RegistryClient {
    /// Build a client with the default `reqwest` transport against real
    /// Docker Hub, cache at the resolved cache path, and the spec's
    /// 1-hour TTL.
    pub fn new() -> crate::Result<Self> {
        let path = cache_path()?;
        let transport: Box<dyn RegistryTransport> = Box::new(HttpTransport::new()?);
        Ok(Self::with_transport(transport)
            .with_cache_path(path)
            .with_ttl(DEFAULT_TTL))
    }

    /// Build a client around an arbitrary transport. The returned client
    /// has no on-disk cache and the default TTL — chain `.with_cache_path`
    /// and/or `.with_ttl` as needed.
    #[must_use]
    pub fn with_transport(transport: Box<dyn RegistryTransport>) -> Self {
        Self {
            transport,
            cache: Mutex::new(CacheStore::default()),
            cache_path: None,
            ttl: DEFAULT_TTL,
        }
    }

    /// Attach an on-disk cache path. The file is loaded eagerly so
    /// subsequent calls see any pre-populated entries.
    #[must_use]
    pub fn with_cache_path(mut self, path: PathBuf) -> Self {
        match CacheStore::load_from(&path) {
            Ok(store) => {
                self.cache = Mutex::new(store);
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to load docker-hub-cache.toml; continuing with empty cache"
                );
            }
        }
        self.cache_path = Some(path);
        self
    }

    /// Override the TTL; the default is [`DEFAULT_TTL`] (1 hour).
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// List tags for `repo`. Hits the in-memory cache first (if an entry
    /// is still fresh per the configured TTL) and only calls the
    /// transport on miss or expiry. On miss, a successful response is
    /// written through to both the in-memory and on-disk caches before
    /// returning.
    pub async fn list_tags(&self, repo: &str) -> crate::Result<Vec<String>> {
        let now = Utc::now();
        if let Some(cached) = self.cache_read_tags(repo, now) {
            tracing::debug!(repo, "docker-hub cache hit: tags");
            return Ok(cached);
        }
        tracing::debug!(repo, "docker-hub cache miss: tags");
        let tags = self.transport.list_tags(repo).await?;
        self.cache_write_tags(repo, tags.clone(), now)?;
        Ok(tags)
    }

    /// Resolve `repo:tag` → content digest. Cache/transport policy
    /// identical to [`Self::list_tags`].
    pub async fn resolve_digest(&self, repo: &str, tag: &str) -> crate::Result<String> {
        let now = Utc::now();
        if let Some(cached) = self.cache_read_digest(repo, tag, now) {
            tracing::debug!(repo, tag, "docker-hub cache hit: digest");
            return Ok(cached);
        }
        tracing::debug!(repo, tag, "docker-hub cache miss: digest");
        let digest = self.transport.resolve_digest(repo, tag).await?;
        self.cache_write_digest(repo, tag, digest.clone(), now)?;
        Ok(digest)
    }

    // --- internals ---

    fn cache_read_tags(&self, repo: &str, now: DateTime<Utc>) -> Option<Vec<String>> {
        let guard = self.cache.lock().expect("cache mutex poisoned");
        guard.tag_list_if_fresh(repo, self.ttl, now)
    }

    fn cache_read_digest(&self, repo: &str, tag: &str, now: DateTime<Utc>) -> Option<String> {
        let guard = self.cache.lock().expect("cache mutex poisoned");
        guard.digest_if_fresh(repo, tag, self.ttl, now)
    }

    fn cache_write_tags(
        &self,
        repo: &str,
        tags: Vec<String>,
        now: DateTime<Utc>,
    ) -> crate::Result<()> {
        let mut guard = self.cache.lock().expect("cache mutex poisoned");
        guard.put_tag_list(repo, tags, now);
        self.flush(&guard)
    }

    fn cache_write_digest(
        &self,
        repo: &str,
        tag: &str,
        digest: String,
        now: DateTime<Utc>,
    ) -> crate::Result<()> {
        let mut guard = self.cache.lock().expect("cache mutex poisoned");
        guard.put_digest(repo, tag, digest, now);
        self.flush(&guard)
    }

    fn flush(&self, guard: &CacheStore) -> crate::Result<()> {
        let Some(path) = &self.cache_path else {
            return Ok(());
        };
        guard.save_to(path)
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Counts transport calls so tests can assert cache hit/miss behavior.
    struct CountingTransport {
        calls: Arc<AtomicUsize>,
        digest_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RegistryTransport for CountingTransport {
        async fn list_tags(&self, _repo: &str) -> crate::Result<Vec<String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["1.1.11".to_owned(), "latest".to_owned()])
        }
        async fn resolve_digest(&self, _repo: &str, _tag: &str) -> crate::Result<String> {
            self.digest_calls.fetch_add(1, Ordering::SeqCst);
            Ok("sha256:abc".to_owned())
        }
    }

    fn counting_client(ttl: Duration) -> (RegistryClient, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let digest_calls = Arc::new(AtomicUsize::new(0));
        let transport = Box::new(CountingTransport {
            calls: Arc::clone(&calls),
            digest_calls: Arc::clone(&digest_calls),
        });
        let client = RegistryClient::with_transport(transport).with_ttl(ttl);
        (client, calls, digest_calls)
    }

    // --- CacheStore pure tests ---

    #[test]
    fn cache_store_roundtrip_through_toml() {
        let mut store = CacheStore::default();
        let now = Utc::now();
        store.put_tag_list("foo/bar", vec!["v1".into(), "v2".into()], now);
        store.put_digest("foo/bar", "v1", "sha256:abc".into(), now);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        store.save_to(tmp.path()).unwrap();
        let loaded = CacheStore::load_from(tmp.path()).unwrap();
        assert_eq!(store, loaded);
    }

    #[test]
    fn cache_store_missing_file_yields_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let loaded = CacheStore::load_from(&path).unwrap();
        assert!(loaded.tags.is_empty());
        assert!(loaded.digests.is_empty());
    }

    #[test]
    fn tag_list_if_fresh_respects_ttl() {
        let mut store = CacheStore::default();
        let cached_at = Utc::now() - chrono::Duration::seconds(30);
        store.put_tag_list("foo/bar", vec!["v1".into()], cached_at);

        // 60s TTL, 30s elapsed → fresh.
        assert!(store
            .tag_list_if_fresh("foo/bar", Duration::from_secs(60), Utc::now())
            .is_some());
        // 10s TTL, 30s elapsed → stale.
        assert!(store
            .tag_list_if_fresh("foo/bar", Duration::from_secs(10), Utc::now())
            .is_none());
    }

    #[test]
    fn digest_if_fresh_keys_by_repo_tag_pair() {
        let mut store = CacheStore::default();
        let now = Utc::now();
        store.put_digest("foo/bar", "v1", "sha256:a".into(), now);
        store.put_digest("foo/bar", "v2", "sha256:b".into(), now);

        assert_eq!(
            store.digest_if_fresh("foo/bar", "v1", Duration::from_secs(60), now),
            Some("sha256:a".into())
        );
        assert_eq!(
            store.digest_if_fresh("foo/bar", "v2", Duration::from_secs(60), now),
            Some("sha256:b".into())
        );
        assert!(store
            .digest_if_fresh("foo/bar", "v3", Duration::from_secs(60), now)
            .is_none());
    }

    // --- RegistryClient behavior ---

    #[tokio::test]
    async fn list_tags_hits_transport_once_under_ttl() {
        let (client, calls, _) = counting_client(Duration::from_secs(3600));
        let first = client.list_tags("foo/bar").await.unwrap();
        let second = client.list_tags("foo/bar").await.unwrap();
        assert_eq!(first, second);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "second call must be cached"
        );
    }

    #[tokio::test]
    async fn list_tags_refetches_after_zero_ttl() {
        let (client, calls, _) = counting_client(Duration::from_secs(0));
        client.list_tags("foo/bar").await.unwrap();
        client.list_tags("foo/bar").await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "zero TTL means every call is a miss",
        );
    }

    #[tokio::test]
    async fn resolve_digest_cache_keys_independently_per_tag() {
        let (client, _, digest_calls) = counting_client(Duration::from_secs(3600));
        client.resolve_digest("foo/bar", "v1").await.unwrap();
        client.resolve_digest("foo/bar", "v2").await.unwrap();
        client.resolve_digest("foo/bar", "v1").await.unwrap();
        assert_eq!(
            digest_calls.load(Ordering::SeqCst),
            2,
            "v1 should only be fetched once; v2 fetched once; total = 2",
        );
    }

    #[tokio::test]
    async fn cache_is_written_to_disk_when_path_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.toml");
        let (_, calls, _) = counting_client(Duration::from_secs(3600));
        let client = RegistryClient::with_transport(Box::new(CountingTransport {
            calls: Arc::clone(&calls),
            digest_calls: Arc::new(AtomicUsize::new(0)),
        }))
        .with_cache_path(path.clone())
        .with_ttl(Duration::from_secs(3600));

        client.list_tags("foo/bar").await.unwrap();
        assert!(path.is_file(), "cache file should exist after a miss");

        let reloaded = CacheStore::load_from(&path).unwrap();
        assert!(reloaded.tags.contains_key("foo/bar"));
    }
}
