//! Relay lifecycle state machines: **bootstrap / check-updates / update
//! (with rollback) / status**. Spec §5.1.
//!
//! Every remote command is executed via [`crate::session::RemoteExec`] —
//! no local shell-out, no scp'd scripts. Argument strings go through the
//! allowlist validators in [`commands`] before concatenation, so
//! shell-meta injection is impossible at the API boundary.
//!
//! # Test strategy
//!
//! The orchestrator holds a `&dyn RemoteExec`, so integration tests
//! substitute a stateful in-memory mock for the transport (no real SSH,
//! no real Docker). The three integration tests in
//! `tests/relay_lifecycle_mock.rs` cover: bootstrap onto a blank host,
//! check-updates against a bootstrapped host, and a happy-path update.
//! `tests/relay_rollback.rs` is gated on `RUSTMOTE_INTEGRATION_DOCKER=1`
//! and exercises the full orchestrator against a local Docker engine.

pub mod commands;
pub mod state;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::error::RustmoteError;
use crate::registry_client::RegistryClient;
use crate::session::{ExecOutput, RemoteExec};

pub use state::{ImagePin, InstallMetadata, RelayState};

// -----------------------------------------------------------------------------
// Defaults + constants
// -----------------------------------------------------------------------------

/// Default install root on the remote host (spec §5.1.1 step 4).
pub const DEFAULT_INSTALL_PATH: &str = "/opt/rustmote-relay";

/// Default Docker Hub repository for the relay images.
pub const DEFAULT_REPO: &str = "rustdesk/rustdesk-server";

/// Default tag pinned in the shipped compose template (spec §5.0).
pub const DEFAULT_TAG: &str = "1.1.11";

/// Services the compose template defines. Order matters for readability
/// only — the state machine is service-order-insensitive.
pub const DEFAULT_SERVICES: &[&str] = &["hbbs", "hbbr"];

/// hbbs signalling port (spec §5.0).
pub const HBBS_PORT: u16 = 21_116;

/// hbbr relay port (spec §5.0).
pub const HBBR_PORT: u16 = 21_117;

/// Health-check loop timeout. Spec §5.1.3 step 10 says "30 seconds".
pub const DEFAULT_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Age threshold above which pre-update backups become eligible for GC.
/// Spec §5.1.3 step 13.
pub const BACKUP_RETENTION_DAYS: i64 = 7;

/// Minimum number of pre-update backup directories preserved regardless
/// of age — i.e. the three most recent are always kept (spec §5.1.3 step 13).
pub const MIN_RETAINED_BACKUPS: usize = 3;

// -----------------------------------------------------------------------------
// Public types
// -----------------------------------------------------------------------------

/// OS hint. Auto-detection via `/etc/os-release` is the default; callers
/// pass `Some(..)` to override when detection fails or differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Debian,
    Ubuntu,
    Arch,
}

/// Input to [`RelayLifecycle::bootstrap`].
#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    pub install_path: PathBuf,
    pub compose_template: String,
    pub env_contents: String,
    pub repo: String,
    pub tag: String,
    pub services: Vec<String>,
    pub rustmote_version: String,
    pub os_hint: Option<OsKind>,
}

impl BootstrapOptions {
    /// Construct with spec-default paths, repo, tag, and services. Caller
    /// supplies the compose template text and `.env` contents — which
    /// live in `docker/relay/` (Phase 14) — plus the current rustmote
    /// crate version.
    #[must_use]
    pub fn new(compose_template: String, env_contents: String, rustmote_version: String) -> Self {
        Self {
            install_path: PathBuf::from(DEFAULT_INSTALL_PATH),
            compose_template,
            env_contents,
            repo: DEFAULT_REPO.to_string(),
            tag: DEFAULT_TAG.to_string(),
            services: DEFAULT_SERVICES.iter().map(ToString::to_string).collect(),
            rustmote_version,
            os_hint: None,
        }
    }
}

/// Result returned by [`RelayLifecycle::bootstrap`].
#[derive(Debug, Clone)]
pub struct BootstrapReport {
    pub install_path: PathBuf,
    pub state: RelayState,
    pub relay_public_key: Option<String>,
    /// True if the relay was already installed and this call was a no-op.
    pub already_installed: bool,
    pub detected_os: OsKind,
}

/// One row in [`CheckUpdatesReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageUpdate {
    pub service: String,
    pub repo: String,
    pub current_tag: String,
    pub current_digest: String,
    pub latest_tag: String,
    pub latest_digest: String,
}

impl ImageUpdate {
    /// True if the resolved latest digest differs from the installed one.
    #[must_use]
    pub fn update_available(&self) -> bool {
        self.current_digest != self.latest_digest
    }
}

/// Result returned by [`RelayLifecycle::check_updates`].
#[derive(Debug, Clone)]
pub struct CheckUpdatesReport {
    pub install_path: PathBuf,
    pub images: Vec<ImageUpdate>,
}

impl CheckUpdatesReport {
    /// True if any service has a newer digest available.
    #[must_use]
    pub fn any_update_available(&self) -> bool {
        self.images.iter().any(ImageUpdate::update_available)
    }
}

/// Input to [`RelayLifecycle::update`].
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    pub install_path: PathBuf,
    pub compose_template: String,
    pub services: Vec<String>,
    pub repo: String,
    /// Non-interactive confirmation flag. Spec §5.1.3 step 4: if stdin
    /// is not a TTY and this is false, [`RelayLifecycle::update`] refuses with
    /// [`RustmoteError::RelayUpdateNotConfirmed`].
    pub assume_yes: bool,
    /// Caller-measured TTY status of stdin. The core module doesn't
    /// inspect `stdin` itself.
    pub is_tty: bool,
    pub skip_backup: bool,
    pub rustmote_version: String,
}

/// Result returned by [`RelayLifecycle::update`].
#[derive(Debug, Clone)]
pub struct UpdateReport {
    pub install_path: PathBuf,
    pub changed: bool,
    pub new_state: RelayState,
    pub backup_dir: Option<PathBuf>,
    /// True when the new images failed health checks and the orchestrator
    /// restored the backup. The error path returns
    /// [`RustmoteError::RelayHealthCheckFailed`]; this field surfaces in
    /// successful calls that never exercised rollback.
    pub rolled_back: bool,
    pub gc_deleted: Vec<PathBuf>,
}

/// Container-status row used by [`StatusReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerStatus {
    pub service: String,
    pub state: String,
    pub image: Option<String>,
}

/// Result returned by [`RelayLifecycle::status`].
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub install_path: PathBuf,
    pub state: RelayState,
    pub containers: Vec<ContainerStatus>,
    pub relay_public_key: Option<String>,
}

// -----------------------------------------------------------------------------
// Orchestrator
// -----------------------------------------------------------------------------

/// Clock closure. Injected so tests can pin wall-clock timestamps while
/// production uses [`Utc::now`].
type Clock = Box<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Lifecycle orchestrator — holds a `&dyn RemoteExec` so tests can swap
/// transports without touching the production code path.
pub struct RelayLifecycle<'a> {
    transport: &'a dyn RemoteExec,
    clock: Clock,
    health_check_timeout: Duration,
    health_check_interval: Duration,
}

impl<'a> RelayLifecycle<'a> {
    /// Wrap `transport` in a new orchestrator with production defaults
    /// (real clock, 30-second health-check window, 1-second interval).
    #[must_use]
    pub fn new(transport: &'a dyn RemoteExec) -> Self {
        Self {
            transport,
            clock: Box::new(Utc::now),
            health_check_timeout: DEFAULT_HEALTH_CHECK_TIMEOUT,
            health_check_interval: Duration::from_secs(1),
        }
    }

    /// Override the clock used for state-file timestamps. Tests pass a
    /// frozen `now` so `.rustmote-state.toml` content is deterministic.
    #[must_use]
    pub fn with_clock<F>(mut self, clock: F) -> Self
    where
        F: Fn() -> DateTime<Utc> + Send + Sync + 'static,
    {
        self.clock = Box::new(clock);
        self
    }

    /// Override the health-check timeout. Tests set this to a few
    /// milliseconds so rollback paths can be exercised in under a second.
    #[must_use]
    pub fn with_health_check_timeout(mut self, t: Duration) -> Self {
        self.health_check_timeout = t;
        self
    }

    /// Override the health-check poll interval (default 1 s).
    #[must_use]
    pub fn with_health_check_interval(mut self, i: Duration) -> Self {
        self.health_check_interval = i;
        self
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    // -------------------------------------------------------------------------
    // Bootstrap
    // -------------------------------------------------------------------------

    /// Install the relay on a fresh host. Spec §5.1.1.
    ///
    /// # Errors
    /// Propagates transport errors, surfaces
    /// [`RustmoteError::RelayAlreadyInstalled`] on a re-run with a
    /// foreign install at `install_path`, [`RustmoteError::DockerEngineNotInstalled`]
    /// if docker is absent, [`RustmoteError::DockerComposeV1Detected`]
    /// on v1-only hosts, and [`RustmoteError::RelayHealthCheckFailed`]
    /// if the freshly-started containers fail their health probes.
    pub async fn bootstrap(
        &self,
        opts: &BootstrapOptions,
        registry: &RegistryClient,
    ) -> crate::Result<BootstrapReport> {
        // --- OS detection ---------------------------------------------------
        let detected_os = match opts.os_hint {
            Some(o) => o,
            None => self.detect_os().await?,
        };

        // --- Docker engine + compose v2 -------------------------------------
        self.ensure_docker_stack().await?;

        // --- Idempotence check: existing install ----------------------------
        let state_path = state_path(&opts.install_path);
        let install_exists = self.path_exists(&opts.install_path, true).await?;
        if install_exists {
            let state_exists = self.path_exists(&state_path, false).await?;
            if !state_exists {
                return Err(RustmoteError::RelayForeignInstall(
                    opts.install_path.clone(),
                ));
            }
            // Idempotent re-run: load state, return it verbatim.
            let state_toml = self.read_file(&state_path).await?.stdout_string();
            let state =
                RelayState::from_toml_str(&state_toml, state_path.to_string_lossy().as_ref())?;
            let relay_public_key = self.read_public_key(&opts.install_path).await.ok();
            return Ok(BootstrapReport {
                install_path: opts.install_path.clone(),
                state,
                relay_public_key,
                already_installed: true,
                detected_os,
            });
        }

        // --- Fresh install: resolve digests first, then write files ---------
        let now = self.now();
        let mut pins = Vec::with_capacity(opts.services.len());
        for service in &opts.services {
            let digest = registry.resolve_digest(&opts.repo, &opts.tag).await?;
            pins.push(ImagePin {
                service: service.clone(),
                repo: opts.repo.clone(),
                tag: opts.tag.clone(),
                digest,
                pinned_at: now,
            });
        }

        let compose = render_compose_with_pins(&opts.compose_template, &pins)?;
        let state = RelayState::new_bootstrap(now, opts.rustmote_version.clone(), pins);

        // Layout: install root + data + backups dirs, then files.
        self.exec_ok(commands::mkdir_p(&opts.install_path)?).await?;
        self.exec_ok(commands::mkdir_p(&data_path(&opts.install_path))?)
            .await?;
        self.exec_ok(commands::mkdir_p(&backups_path(&opts.install_path))?)
            .await?;

        self.exec_ok(commands::write_file(
            &compose_path(&opts.install_path),
            compose.as_bytes(),
        )?)
        .await?;
        self.exec_ok(commands::write_file(
            &env_path(&opts.install_path),
            opts.env_contents.as_bytes(),
        )?)
        .await?;
        self.exec_ok(commands::write_file(
            &state_path,
            state.to_toml_string()?.as_bytes(),
        )?)
        .await?;

        // Bring the stack up + health-check. No rollback on bootstrap:
        // a failed bootstrap leaves no previous-working-version to restore.
        self.exec_ok(commands::compose_up(&opts.install_path)?)
            .await?;
        self.health_check(&[HBBS_PORT, HBBR_PORT]).await?;

        let relay_public_key = self.read_public_key(&opts.install_path).await.ok();

        Ok(BootstrapReport {
            install_path: opts.install_path.clone(),
            state,
            relay_public_key,
            already_installed: false,
            detected_os,
        })
    }

    // -------------------------------------------------------------------------
    // check_updates
    // -------------------------------------------------------------------------

    /// Read state, query Docker Hub for the latest semver tag's digest,
    /// and diff. Read-only. Spec §5.1.2.
    ///
    /// # Errors
    /// Propagates transport errors, returns [`RustmoteError::RelayNotInstalled`]
    /// if the state file is absent, and [`RustmoteError::RegistryApi`] on
    /// Docker Hub failures.
    pub async fn check_updates(
        &self,
        install_path: &Path,
        registry: &RegistryClient,
    ) -> crate::Result<CheckUpdatesReport> {
        let state = self.load_state(install_path).await?;

        let mut updates = Vec::with_capacity(state.images.len());
        for pin in &state.images {
            let tags = registry.list_tags(&pin.repo).await?;
            let latest_tag = pick_latest_semver(&tags).unwrap_or_else(|| pin.tag.clone());
            let latest_digest = registry.resolve_digest(&pin.repo, &latest_tag).await?;
            updates.push(ImageUpdate {
                service: pin.service.clone(),
                repo: pin.repo.clone(),
                current_tag: pin.tag.clone(),
                current_digest: pin.digest.clone(),
                latest_tag,
                latest_digest,
            });
        }

        Ok(CheckUpdatesReport {
            install_path: install_path.to_path_buf(),
            images: updates,
        })
    }

    // -------------------------------------------------------------------------
    // update
    // -------------------------------------------------------------------------

    /// Pull, restart, health-check, rollback on failure, GC old backups.
    /// Spec §5.1.3.
    ///
    /// # Errors
    /// Returns [`RustmoteError::RelayUpdateNotConfirmed`] if stdin is not
    /// a TTY and `assume_yes` is false. Returns
    /// [`RustmoteError::RelayHealthCheckFailed`] if the new containers
    /// fail their health probes and the orchestrator successfully
    /// restored the backup (the rollback path runs before the error
    /// bubbles up).
    pub async fn update(
        &self,
        opts: &UpdateOptions,
        registry: &RegistryClient,
    ) -> crate::Result<UpdateReport> {
        if !opts.assume_yes && !opts.is_tty {
            return Err(RustmoteError::RelayUpdateNotConfirmed);
        }

        // Step 1: check what's available. If nothing, short-circuit.
        let report = self.check_updates(&opts.install_path, registry).await?;
        if !report.any_update_available() {
            let state = self.load_state(&opts.install_path).await?;
            return Ok(UpdateReport {
                install_path: opts.install_path.clone(),
                changed: false,
                new_state: state,
                backup_dir: None,
                rolled_back: false,
                gc_deleted: vec![],
            });
        }

        // Step 2: pre-update backup (unless skipped).
        let now = self.now();
        let backup_dir = if opts.skip_backup {
            None
        } else {
            let dir = backup_dir_for(&opts.install_path, now);
            self.exec_ok(commands::mkdir_p(&dir)?).await?;
            self.exec_ok(commands::copy_file(
                &compose_path(&opts.install_path),
                &dir.join("docker-compose.yml"),
            )?)
            .await?;
            self.exec_ok(commands::copy_file(
                &state_path(&opts.install_path),
                &dir.join(".rustmote-state.toml"),
            )?)
            .await?;
            // `docker compose config` output captured as a plaintext
            // snapshot. Failure here is logged but non-fatal — the two
            // authoritative files are already copied.
            let cfg = self
                .transport
                .exec(&commands::compose_config(&opts.install_path)?)
                .await?;
            if cfg.exit_code == 0 {
                self.exec_ok(commands::write_file(
                    &dir.join("compose-config.yml"),
                    &cfg.stdout,
                )?)
                .await?;
            }
            Some(dir)
        };

        // Step 3: write new compose + state, pull, up.
        let new_pins: Vec<ImagePin> = report
            .images
            .iter()
            .map(|u| ImagePin {
                service: u.service.clone(),
                repo: u.repo.clone(),
                tag: u.latest_tag.clone(),
                digest: u.latest_digest.clone(),
                pinned_at: now,
            })
            .collect();
        let mut new_state = self.load_state(&opts.install_path).await?;
        new_state.apply_update(new_pins.clone(), now);
        let new_compose = render_compose_with_pins(&opts.compose_template, &new_pins)?;

        self.exec_ok(commands::write_file(
            &compose_path(&opts.install_path),
            new_compose.as_bytes(),
        )?)
        .await?;
        self.exec_ok(commands::write_file(
            &state_path(&opts.install_path),
            new_state.to_toml_string()?.as_bytes(),
        )?)
        .await?;
        self.exec_ok(commands::compose_pull(&opts.install_path)?)
            .await?;
        self.exec_ok(commands::compose_up(&opts.install_path)?)
            .await?;

        // Step 4: health check. On failure -> rollback.
        if let Err(e) = self.health_check(&[HBBS_PORT, HBBR_PORT]).await {
            if let Some(dir) = &backup_dir {
                self.rollback(&opts.install_path, dir).await?;
                // Rollback succeeded — surface the failure but tell the
                // caller we did land safely. The error variant carries
                // the "rolled back" context in its message.
                return Err(RustmoteError::RelayHealthCheckFailed);
            }
            return Err(e);
        }

        // Step 5: GC old backups.
        let gc_deleted = self.gc_backups(&opts.install_path, now).await?;

        Ok(UpdateReport {
            install_path: opts.install_path.clone(),
            changed: true,
            new_state,
            backup_dir,
            rolled_back: false,
            gc_deleted,
        })
    }

    // -------------------------------------------------------------------------
    // status
    // -------------------------------------------------------------------------

    /// Read state + `docker compose ps` output + relay public key. Spec §5.1.4.
    ///
    /// # Errors
    /// Propagates transport errors and [`RustmoteError::RelayNotInstalled`].
    pub async fn status(&self, install_path: &Path) -> crate::Result<StatusReport> {
        let state = self.load_state(install_path).await?;

        let ps = self
            .transport
            .exec(&commands::compose_ps_json(install_path)?)
            .await?;
        let containers = if ps.exit_code == 0 {
            parse_compose_ps_json(&ps.stdout_string())
        } else {
            vec![]
        };

        let relay_public_key = self.read_public_key(install_path).await.ok();

        Ok(StatusReport {
            install_path: install_path.to_path_buf(),
            state,
            containers,
            relay_public_key,
        })
    }

    // -------------------------------------------------------------------------
    // Internals
    // -------------------------------------------------------------------------

    async fn exec_ok(&self, command: String) -> crate::Result<ExecOutput> {
        let out = self.transport.exec(&command).await?;
        out.ok_for(&command)
    }

    async fn path_exists(&self, path: &Path, directory: bool) -> crate::Result<bool> {
        let cmd = if directory {
            commands::test_dir_exists(path)?
        } else {
            commands::test_file_exists(path)?
        };
        let out = self.transport.exec(&cmd).await?;
        Ok(out.exit_code == 0)
    }

    async fn read_file(&self, path: &Path) -> crate::Result<ExecOutput> {
        self.exec_ok(commands::cat_file(path)?).await
    }

    async fn read_public_key(&self, install_path: &Path) -> crate::Result<String> {
        let p = data_path(install_path).join("id_ed25519.pub");
        let out = self.read_file(&p).await?;
        Ok(out.stdout_string().trim().to_owned())
    }

    async fn load_state(&self, install_path: &Path) -> crate::Result<RelayState> {
        let p = state_path(install_path);
        if !self.path_exists(&p, false).await? {
            return Err(RustmoteError::RelayNotInstalled(
                install_path.to_string_lossy().into_owned(),
            ));
        }
        let raw = self.read_file(&p).await?.stdout_string();
        RelayState::from_toml_str(&raw, p.to_string_lossy().as_ref())
    }

    async fn detect_os(&self) -> crate::Result<OsKind> {
        let out = self.exec_ok(commands::read_os_release()).await?;
        parse_os_release(&out.stdout_string()).ok_or_else(|| {
            RustmoteError::RelayUnsupportedOs("no recognized ID in /etc/os-release".to_string())
        })
    }

    async fn ensure_docker_stack(&self) -> crate::Result<()> {
        // 1. docker binary present?
        let docker_present = self
            .transport
            .exec(&commands::check_docker_present())
            .await?;
        if docker_present.exit_code != 0 {
            return Err(RustmoteError::DockerEngineNotInstalled);
        }
        // 2. `docker compose` (v2) present?
        let v2 = self
            .transport
            .exec(&commands::check_docker_compose_v2())
            .await?;
        if v2.exit_code != 0 {
            // Is it the v1 binary? Surface a distinct error for the
            // migration hint.
            let v1 = self
                .transport
                .exec(&commands::check_docker_compose_v1())
                .await?;
            if v1.exit_code == 0 {
                return Err(RustmoteError::DockerComposeV1Detected);
            }
            return Err(RustmoteError::DockerEngineNotInstalled);
        }
        Ok(())
    }

    async fn health_check(&self, ports: &[u16]) -> crate::Result<()> {
        let deadline = tokio::time::Instant::now() + self.health_check_timeout;
        loop {
            let mut all_up = true;
            for p in ports {
                let out = self.transport.exec(&commands::tcp_probe(*p)).await?;
                if out.exit_code != 0 {
                    all_up = false;
                    break;
                }
            }
            if all_up {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(RustmoteError::RelayHealthCheckFailed);
            }
            tokio::time::sleep(self.health_check_interval).await;
        }
    }

    async fn rollback(&self, install_path: &Path, backup_dir: &Path) -> crate::Result<()> {
        self.exec_ok(commands::copy_file(
            &backup_dir.join("docker-compose.yml"),
            &compose_path(install_path),
        )?)
        .await?;
        self.exec_ok(commands::copy_file(
            &backup_dir.join(".rustmote-state.toml"),
            &state_path(install_path),
        )?)
        .await?;
        self.exec_ok(commands::compose_up(install_path)?).await?;
        // Verify the rollback itself is healthy. If *this* also fails
        // we surface RelayHealthCheckFailed but the original error is
        // already the one the caller cares about.
        self.health_check(&[HBBS_PORT, HBBR_PORT]).await?;
        Ok(())
    }

    async fn gc_backups(
        &self,
        install_path: &Path,
        now: DateTime<Utc>,
    ) -> crate::Result<Vec<PathBuf>> {
        let backups = backups_path(install_path);
        // List backup directories with mtime via `find`. Output format:
        // `<epoch-seconds>\t<path>` per line — stable across GNU find.
        let cmd = format!(
            "find {} -mindepth 1 -maxdepth 1 -type d -name 'pre-update-*' -printf '%T@\\t%p\\n'",
            commands::validate_remote_path(&backups)?
        );
        let out = self.transport.exec(&cmd).await?;
        if out.exit_code != 0 {
            // No backups dir yet (fresh install never updated). Ignore.
            return Ok(vec![]);
        }
        let mut entries: Vec<(i64, PathBuf)> = out
            .stdout_string()
            .lines()
            .filter_map(|line| {
                let (ts, path) = line.split_once('\t')?;
                let epoch = ts.split('.').next()?.parse::<i64>().ok()?;
                Some((epoch, PathBuf::from(path)))
            })
            .collect();
        // Sort newest first.
        entries.sort_by_key(|e| std::cmp::Reverse(e.0));

        let cutoff = now.timestamp() - BACKUP_RETENTION_DAYS * 24 * 3600;
        let mut deleted = vec![];
        for (idx, (epoch, path)) in entries.iter().enumerate() {
            if idx < MIN_RETAINED_BACKUPS {
                continue;
            }
            if *epoch < cutoff {
                let p = commands::validate_remote_path(path)?;
                self.exec_ok(format!("rm -rf {p}")).await?;
                deleted.push(path.clone());
            }
        }
        Ok(deleted)
    }
}

// -----------------------------------------------------------------------------
// Path helpers
// -----------------------------------------------------------------------------

/// `<install>/docker-compose.yml`.
#[must_use]
pub fn compose_path(install_path: &Path) -> PathBuf {
    install_path.join("docker-compose.yml")
}

/// `<install>/.env`.
#[must_use]
pub fn env_path(install_path: &Path) -> PathBuf {
    install_path.join(".env")
}

/// `<install>/.rustmote-state.toml`.
#[must_use]
pub fn state_path(install_path: &Path) -> PathBuf {
    install_path.join(".rustmote-state.toml")
}

/// `<install>/data`.
#[must_use]
pub fn data_path(install_path: &Path) -> PathBuf {
    install_path.join("data")
}

/// `<install>/backups`.
#[must_use]
pub fn backups_path(install_path: &Path) -> PathBuf {
    install_path.join("backups")
}

/// `<install>/backups/pre-update-<iso-suffix>`.
#[must_use]
pub fn backup_dir_for(install_path: &Path, at: DateTime<Utc>) -> PathBuf {
    // Replace `:` with `-` so the path is safe everywhere (Windows file
    // systems reject `:`, and it's easier to eyeball in an ls listing).
    let suffix = at.format("%Y-%m-%dT%H-%M-%SZ").to_string();
    backups_path(install_path).join(format!("pre-update-{suffix}"))
}

// -----------------------------------------------------------------------------
// Parsers / renderers
// -----------------------------------------------------------------------------

/// Extract the first `ID=...` line from `/etc/os-release` and map to an
/// [`OsKind`]. Quoted values and `ID_LIKE` fallback are handled.
#[must_use]
pub fn parse_os_release(raw: &str) -> Option<OsKind> {
    let mut id: Option<String> = None;
    let mut id_like: Option<String> = None;
    for line in raw.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ID=") {
            id = Some(rest.trim_matches('"').to_lowercase());
        } else if let Some(rest) = line.strip_prefix("ID_LIKE=") {
            id_like = Some(rest.trim_matches('"').to_lowercase());
        }
    }
    let match_id = |s: &str| -> Option<OsKind> {
        match s {
            "debian" => Some(OsKind::Debian),
            "ubuntu" => Some(OsKind::Ubuntu),
            "arch" | "archlinux" => Some(OsKind::Arch),
            _ => None,
        }
    };
    id.as_deref()
        .and_then(match_id)
        .or_else(|| id_like.as_deref()?.split_whitespace().find_map(match_id))
}

/// Given the compose template (with tag-only `image:` lines) and a set
/// of resolved pins keyed by service, rewrite each `image:` line to a
/// tag+digest pin per spec §5.1.1 step 6.
///
/// We look for the pattern `image: {repo}:{tag}` (arbitrary leading
/// whitespace) and replace it with `image: {repo}:{tag}@{digest}`. The
/// compose template ships with a single repo so ambiguity is not a
/// concern — but we replace every occurrence to be robust.
///
/// # Errors
/// Propagates [`commands::pinned_image_ref`] validation failures.
pub fn render_compose_with_pins(template: &str, pins: &[ImagePin]) -> crate::Result<String> {
    let mut out = template.to_string();
    let mut seen: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
    for pin in pins {
        let key = (pin.repo.clone(), pin.tag.clone(), pin.digest.clone());
        if !seen.insert(key) {
            continue;
        }
        let search = format!("image: {}:{}", pin.repo, pin.tag);
        let replace = format!(
            "image: {}",
            commands::pinned_image_ref(&pin.repo, &pin.tag, &pin.digest)?
        );
        out = out.replace(&search, &replace);
    }
    Ok(out)
}

/// Pick the highest dotted-numeric tag (e.g. `1.1.14` beats `1.1.11`).
/// Non-numeric tags (`latest`, `master`) are ignored. Returns `None` if
/// no tag matches the semver-ish pattern.
#[must_use]
pub fn pick_latest_semver(tags: &[String]) -> Option<String> {
    let mut best: Option<(Vec<u64>, String)> = None;
    for t in tags {
        if let Some(parts) = parse_dotted_numeric(t) {
            match &best {
                None => best = Some((parts, t.clone())),
                Some((cur, _)) if parts > *cur => best = Some((parts, t.clone())),
                _ => {}
            }
        }
    }
    best.map(|(_, s)| s)
}

fn parse_dotted_numeric(t: &str) -> Option<Vec<u64>> {
    let parts: Vec<u64> = t
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

/// Parse the stdout of `docker compose ps --format json`. Compose v2
/// may emit either a JSON array or one object per line (NDJSON). Both
/// shapes are handled.
#[must_use]
pub fn parse_compose_ps_json(raw: &str) -> Vec<ContainerStatus> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    let try_array: std::result::Result<Value, _> = serde_json::from_str(trimmed);
    match try_array {
        Ok(Value::Array(items)) => {
            for item in items {
                if let Some(cs) = row_from_value(&item) {
                    out.push(cs);
                }
            }
        }
        _ => {
            for line in trimmed.lines() {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if let Some(cs) = row_from_value(&v) {
                        out.push(cs);
                    }
                }
            }
        }
    }
    out
}

fn row_from_value(v: &Value) -> Option<ContainerStatus> {
    let service = v.get("Service")?.as_str()?.to_owned();
    let state = v
        .get("State")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let image = v
        .get("Image")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Some(ContainerStatus {
        service,
        state,
        image,
    })
}

// -----------------------------------------------------------------------------
// Unit tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_os_release_recognizes_debian_ubuntu_arch() {
        assert_eq!(
            parse_os_release("NAME=\"Debian GNU/Linux\"\nID=debian\n"),
            Some(OsKind::Debian)
        );
        assert_eq!(
            parse_os_release("NAME=Ubuntu\nID=ubuntu\nID_LIKE=debian\n"),
            Some(OsKind::Ubuntu)
        );
        assert_eq!(
            parse_os_release("NAME=\"Arch Linux\"\nID=arch\n"),
            Some(OsKind::Arch)
        );
    }

    #[test]
    fn parse_os_release_falls_back_to_id_like_for_derivatives() {
        assert_eq!(
            parse_os_release("NAME=Raspbian\nID=raspbian\nID_LIKE=\"debian\"\n"),
            Some(OsKind::Debian)
        );
    }

    #[test]
    fn parse_os_release_returns_none_on_unknown() {
        assert_eq!(parse_os_release("NAME=Plan9\nID=plan9\n"), None);
        assert_eq!(parse_os_release(""), None);
    }

    #[test]
    fn pick_latest_semver_prefers_numeric_tags_over_latest() {
        let tags: Vec<String> = ["1.1.11", "1.1.14", "latest", "master"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(pick_latest_semver(&tags).as_deref(), Some("1.1.14"));
    }

    #[test]
    fn pick_latest_semver_returns_none_when_nothing_numeric() {
        let tags: Vec<String> = ["latest", "edge"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert!(pick_latest_semver(&tags).is_none());
    }

    #[test]
    fn pick_latest_semver_handles_single_component() {
        let tags = vec!["1".to_string(), "2".to_string(), "10".to_string()];
        assert_eq!(pick_latest_semver(&tags).as_deref(), Some("10"));
    }

    #[test]
    fn render_compose_pins_both_services() {
        let template = r"services:
  hbbs:
    image: rustdesk/rustdesk-server:1.1.11
  hbbr:
    image: rustdesk/rustdesk-server:1.1.11
";
        let pins = vec![
            ImagePin {
                service: "hbbs".into(),
                repo: "rustdesk/rustdesk-server".into(),
                tag: "1.1.11".into(),
                digest: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .into(),
                pinned_at: Utc::now(),
            },
            ImagePin {
                service: "hbbr".into(),
                repo: "rustdesk/rustdesk-server".into(),
                tag: "1.1.11".into(),
                digest: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .into(),
                pinned_at: Utc::now(),
            },
        ];
        let rendered = render_compose_with_pins(template, &pins).unwrap();
        assert!(rendered.contains(
            "image: rustdesk/rustdesk-server:1.1.11@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!rendered.contains("image: rustdesk/rustdesk-server:1.1.11\n"));
    }

    #[test]
    fn parse_compose_ps_accepts_array_shape() {
        let raw = r#"[
            {"Service":"hbbs","State":"running","Image":"rustdesk/rustdesk-server:1.1.11"},
            {"Service":"hbbr","State":"running","Image":"rustdesk/rustdesk-server:1.1.11"}
        ]"#;
        let rows = parse_compose_ps_json(raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].service, "hbbs");
        assert_eq!(rows[0].state, "running");
    }

    #[test]
    fn parse_compose_ps_accepts_ndjson_shape() {
        let raw = concat!(
            r#"{"Service":"hbbs","State":"running","Image":"rustdesk/rustdesk-server:1.1.11"}"#,
            "\n",
            r#"{"Service":"hbbr","State":"exited","Image":"rustdesk/rustdesk-server:1.1.11"}"#,
        );
        let rows = parse_compose_ps_json(raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].state, "exited");
    }

    #[test]
    fn backup_dir_replaces_colons_with_dashes() {
        let t = DateTime::parse_from_rfc3339("2026-04-18T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc);
        let p = backup_dir_for(Path::new("/opt/rustmote-relay"), t);
        assert_eq!(
            p,
            PathBuf::from("/opt/rustmote-relay/backups/pre-update-2026-04-18T12-34-56Z")
        );
    }

    #[test]
    fn image_update_detects_digest_drift() {
        let u = ImageUpdate {
            service: "hbbs".into(),
            repo: "rustdesk/rustdesk-server".into(),
            current_tag: "1.1.11".into(),
            current_digest: "sha256:a".into(),
            latest_tag: "1.1.14".into(),
            latest_digest: "sha256:b".into(),
        };
        assert!(u.update_available());

        let same = ImageUpdate {
            latest_digest: u.current_digest.clone(),
            ..u
        };
        assert!(!same.update_available());
    }
}
