//! Integration test for `relay_lifecycle` against an in-memory mock
//! [`RemoteExec`] transport. Hermetic — no SSH, no Docker, no network.
//!
//! The mock implements a closed set of commands the orchestrator is
//! allowed to emit. Any unrecognized command panics with the full
//! command string, which doubles as a "what did you just ask the server
//! to do?" diagnostic when the orchestrator's output changes.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use rustmote_core::registry_client::{RegistryClient, RegistryTransport};
use rustmote_core::relay_lifecycle::{
    backup_dir_for, backups_path, compose_path, data_path, env_path, state_path, BootstrapOptions,
    OsKind, RelayLifecycle, RelayState, UpdateOptions,
};
use rustmote_core::session::{ExecOutput, RemoteExec};
use rustmote_core::RustmoteError;

// -----------------------------------------------------------------------------
// Scripted Docker Hub transport — supplies digests by tag.
// -----------------------------------------------------------------------------

struct FakeHub {
    tags: Vec<String>,
    tag_to_digest: HashMap<String, String>,
}

#[async_trait]
impl RegistryTransport for FakeHub {
    async fn list_tags(&self, _repo: &str) -> rustmote_core::Result<Vec<String>> {
        Ok(self.tags.clone())
    }
    async fn resolve_digest(&self, _repo: &str, tag: &str) -> rustmote_core::Result<String> {
        self.tag_to_digest.get(tag).cloned().ok_or_else(|| {
            RustmoteError::RegistryApi(format!("fake hub has no digest for tag {tag}"))
        })
    }
}

fn client_with_hub(hub: FakeHub) -> RegistryClient {
    RegistryClient::with_transport(Box::new(hub))
}

// -----------------------------------------------------------------------------
// Mock remote host — in-memory filesystem + "docker compose" stub
// -----------------------------------------------------------------------------

#[derive(Debug, Default)]
struct HostState {
    files: HashMap<PathBuf, Vec<u8>>,
    dirs: std::collections::HashSet<PathBuf>,
    /// Ports that respond to TCP probes. Toggled per test to simulate
    /// healthy / unhealthy relays.
    open_ports: std::collections::HashSet<u16>,
    os_release: String,
    docker_present: bool,
    compose_v2_present: bool,
    compose_v1_present: bool,
    /// If set, the next `docker compose up -d` call should flip the set
    /// of open ports to this collection — simulates "fresh images came
    /// up broken" for the rollback test.
    pub pending_up_ports_swap: Option<std::collections::HashSet<u16>>,
}

impl HostState {
    fn healthy_debian() -> Self {
        let mut s = Self {
            os_release: "NAME=\"Debian GNU/Linux\"\nID=debian\n".into(),
            docker_present: true,
            compose_v2_present: true,
            ..Self::default()
        };
        s.open_ports.insert(21_116);
        s.open_ports.insert(21_117);
        s
    }
}

#[derive(Clone)]
struct MockHost {
    state: Arc<Mutex<HostState>>,
    #[allow(dead_code)]
    call_count: Arc<AtomicUsize>,
    /// When true, panic on any unrecognized exec. When false, return
    /// exit 127 — useful for probing optional commands.
    strict: Arc<AtomicBool>,
}

impl MockHost {
    fn new(initial: HostState) -> Self {
        Self {
            state: Arc::new(Mutex::new(initial)),
            call_count: Arc::new(AtomicUsize::new(0)),
            strict: Arc::new(AtomicBool::new(true)),
        }
    }

    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .files
            .get(&PathBuf::from(path))
            .cloned()
    }
}

fn ok(stdout: impl Into<Vec<u8>>) -> ExecOutput {
    ExecOutput {
        stdout: stdout.into(),
        stderr: vec![],
        exit_code: 0,
    }
}

fn nonzero(exit: u32) -> ExecOutput {
    ExecOutput {
        stdout: vec![],
        stderr: vec![],
        exit_code: exit,
    }
}

#[async_trait]
impl RemoteExec for MockHost {
    #[allow(clippy::too_many_lines)] // test-only command recognizer
    async fn exec(&self, command: &str) -> rustmote_core::Result<ExecOutput> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let mut state = self.state.lock().unwrap();

        // --- OS / docker detection ----------------------------------------
        if command == "cat /etc/os-release" {
            return Ok(ok(state.os_release.as_bytes().to_vec()));
        }
        if command == "command -v docker" {
            return Ok(if state.docker_present {
                ok("/usr/bin/docker")
            } else {
                nonzero(1)
            });
        }
        if command == "docker compose version" {
            return Ok(if state.compose_v2_present {
                ok("Docker Compose version v2.21.0")
            } else {
                nonzero(127)
            });
        }
        if command == "docker-compose --version" {
            return Ok(if state.compose_v1_present {
                ok("docker-compose version 1.29.2")
            } else {
                nonzero(127)
            });
        }

        // --- test -d / test -f --------------------------------------------
        if let Some(p) = command.strip_prefix("test -d ") {
            let exists = state.dirs.contains(&PathBuf::from(p));
            return Ok(if exists { ok("") } else { nonzero(1) });
        }
        if let Some(p) = command.strip_prefix("test -f ") {
            let exists = state.files.contains_key(&PathBuf::from(p));
            return Ok(if exists { ok("") } else { nonzero(1) });
        }

        // --- mkdir -p / cat / cp / rm -------------------------------------
        if let Some(p) = command.strip_prefix("mkdir -p ") {
            state.dirs.insert(PathBuf::from(p));
            return Ok(ok(""));
        }
        if let Some(p) = command.strip_prefix("cat ") {
            let key = PathBuf::from(p);
            return Ok(match state.files.get(&key) {
                Some(v) => ok(v.clone()),
                None => nonzero(1),
            });
        }
        if let Some(rest) = command.strip_prefix("cp -a ") {
            let (src, dst) = rest.split_once(' ').expect("cp needs two args");
            let src = PathBuf::from(src);
            let dst = PathBuf::from(dst);
            let data = state
                .files
                .get(&src)
                .cloned()
                .expect("cp of nonexistent file");
            state.files.insert(dst, data);
            return Ok(ok(""));
        }
        if let Some(p) = command.strip_prefix("rm -rf ") {
            let pb = PathBuf::from(p);
            state.dirs.remove(&pb);
            state.files.retain(|k, _| !k.starts_with(&pb));
            // Remove descendant dirs too.
            state.dirs.retain(|k| !k.starts_with(&pb));
            return Ok(ok(""));
        }

        // --- write_file: printf {b64} | base64 -d > X.tmp && mv X.tmp X && chmod 0644 X
        if let Some(rest) = command.strip_prefix("printf %s ") {
            // Parse: `<b64> | base64 -d > {tmp} && mv {tmp} {dst} && chmod 0644 {dst}`
            let (b64, tail) = rest
                .split_once(" | base64 -d > ")
                .expect("write_file shape");
            let (tmp, tail) = tail.split_once(" && mv ").expect("write_file shape");
            let (_mv_src, tail) = tail.split_once(' ').expect("write_file shape");
            let (dst, _chmod) = tail
                .split_once(" && chmod 0644 ")
                .expect("write_file shape");
            assert!(tmp.ends_with(".rustmote-tmp"));
            let decoded = B64
                .decode(b64)
                .expect("mock received non-base64 payload from write_file");
            state.files.insert(PathBuf::from(dst), decoded);
            return Ok(ok(""));
        }

        // --- docker compose up / pull / config / ps -----------------------
        if command.ends_with("docker compose up -d") {
            if let Some(target) = state.pending_up_ports_swap.take() {
                state.open_ports = target;
            }
            return Ok(ok(""));
        }
        if command.ends_with("docker compose pull") {
            return Ok(ok(""));
        }
        if command.ends_with("docker compose config") {
            return Ok(ok("services: {}\n"));
        }
        if command.ends_with("docker compose ps --format json") {
            let hbbs_img = state
                .files
                .get(&PathBuf::from("/opt/rustmote-relay/docker-compose.yml"))
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .unwrap_or_default();
            let tag = if hbbs_img.contains("1.1.14") {
                "1.1.14"
            } else {
                "1.1.11"
            };
            let body = format!(
                r#"[
                    {{"Service":"hbbs","State":"running","Image":"rustdesk/rustdesk-server:{tag}"}},
                    {{"Service":"hbbr","State":"running","Image":"rustdesk/rustdesk-server:{tag}"}}
                ]"#,
            );
            return Ok(ok(body));
        }

        // --- TCP probe ----------------------------------------------------
        if let Some(rest) = command.strip_prefix("bash -c 'exec 3<>/dev/tcp/127.0.0.1/") {
            let port: u16 = rest
                .split('\'')
                .next()
                .unwrap()
                .parse()
                .expect("port in tcp_probe");
            return Ok(if state.open_ports.contains(&port) {
                ok("")
            } else {
                nonzero(1)
            });
        }

        // --- find backups -------------------------------------------------
        if command.starts_with("find ") && command.contains("pre-update-") {
            // Emit <epoch>\t<path> for each backup dir currently in our
            // in-memory filesystem that lives under the backups dir.
            let mut lines = String::new();
            for d in &state.dirs {
                if let Some(stem) = d.file_name().and_then(|s| s.to_str()) {
                    if stem.starts_with("pre-update-") {
                        // Parse `pre-update-2026-04-18T12-34-56Z`.
                        let suffix = stem.trim_start_matches("pre-update-");
                        let t = reinflate_iso(suffix).unwrap_or_else(Utc::now);
                        writeln!(lines, "{}.0\t{}", t.timestamp(), d.display()).unwrap();
                    }
                }
            }
            return Ok(ok(lines));
        }

        // --- catch-all: strict by default ---------------------------------
        assert!(
            !self.strict.load(Ordering::SeqCst),
            "MockHost: unhandled command: {command}"
        );
        Ok(nonzero(127))
    }
}

/// Turn `2026-04-18T12-34-56Z` back into a real `DateTime<Utc>` for
/// `find`'s mtime output.
fn reinflate_iso(s: &str) -> Option<DateTime<Utc>> {
    // Format: YYYY-MM-DDTHH-MM-SSZ
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let date: Vec<&str> = date.splitn(3, '-').collect();
    let time: Vec<&str> = time.splitn(3, '-').collect();
    if date.len() != 3 || time.len() != 3 {
        return None;
    }
    let y: i32 = date[0].parse().ok()?;
    let mo: u32 = date[1].parse().ok()?;
    let d: u32 = date[2].parse().ok()?;
    let h: u32 = time[0].parse().ok()?;
    let mi: u32 = time[1].parse().ok()?;
    let se: u32 = time[2].parse().ok()?;
    Utc.with_ymd_and_hms(y, mo, d, h, mi, se).single()
}

// -----------------------------------------------------------------------------
// Fixtures
// -----------------------------------------------------------------------------

fn compose_template() -> String {
    r"services:
  hbbs:
    image: rustdesk/rustdesk-server:1.1.11
    container_name: rustmote-hbbs
  hbbr:
    image: rustdesk/rustdesk-server:1.1.11
    container_name: rustmote-hbbr
"
    .to_string()
}

fn bootstrap_opts() -> BootstrapOptions {
    BootstrapOptions::new(
        compose_template(),
        "RUSTMOTE_RELAY=1\n".to_string(),
        "0.1.0".to_string(),
    )
}

fn frozen(t: DateTime<Utc>) -> impl Fn() -> DateTime<Utc> + Send + Sync + 'static {
    move || t
}

fn base_clock() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 18, 12, 34, 56).unwrap()
}

const DIGEST_V1: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const DIGEST_V2: &str = "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[tokio::test]
async fn bootstrap_writes_compose_state_env_and_starts_stack() {
    let host = MockHost::new(HostState::healthy_debian());
    // id_ed25519.pub appears after `docker compose up` would run in
    // reality; for the mock we seed it as if generation succeeded.
    host.state.lock().unwrap().files.insert(
        data_path(&PathBuf::from("/opt/rustmote-relay")).join("id_ed25519.pub"),
        b"ssh-ed25519 AAAA... rustmote-relay\n".to_vec(),
    );

    let hub = FakeHub {
        tags: vec!["1.1.11".into(), "latest".into()],
        tag_to_digest: HashMap::from([
            ("1.1.11".into(), DIGEST_V1.into()),
            ("latest".into(), DIGEST_V1.into()),
        ]),
    };
    let registry = client_with_hub(hub);

    let lc = RelayLifecycle::new(&host)
        .with_clock(frozen(base_clock()))
        .with_health_check_interval(Duration::from_millis(0))
        .with_health_check_timeout(Duration::from_millis(50));

    let report = lc.bootstrap(&bootstrap_opts(), &registry).await.unwrap();

    assert_eq!(report.detected_os, OsKind::Debian);
    assert!(!report.already_installed);
    assert_eq!(report.state.images.len(), 2);
    for p in &report.state.images {
        assert_eq!(p.tag, "1.1.11");
        assert_eq!(p.digest, DIGEST_V1);
    }
    assert_eq!(
        report.relay_public_key.as_deref(),
        Some("ssh-ed25519 AAAA... rustmote-relay")
    );

    // Compose file was written with the pinned digest.
    let compose = host
        .read_file("/opt/rustmote-relay/docker-compose.yml")
        .expect("compose file should exist");
    let compose = String::from_utf8(compose).unwrap();
    assert!(
        compose.contains(&format!(
            "image: rustdesk/rustdesk-server:1.1.11@{DIGEST_V1}"
        )),
        "compose should carry the digest pin: {compose}"
    );
    // State file parses and contains both services.
    let state_raw = host
        .read_file("/opt/rustmote-relay/.rustmote-state.toml")
        .expect("state file should exist");
    let parsed =
        RelayState::from_toml_str(std::str::from_utf8(&state_raw).unwrap(), "state.toml").unwrap();
    assert_eq!(parsed.install.bootstrapped_by_rustmote_version, "0.1.0");
    assert_eq!(parsed.install.last_updated_at, None);
    assert_eq!(parsed.images.len(), 2);
}

#[tokio::test]
async fn bootstrap_is_idempotent_and_returns_existing_state() {
    let host = MockHost::new(HostState::healthy_debian());
    // Pre-seed as if bootstrap already ran: install dir + state file.
    {
        let mut s = host.state.lock().unwrap();
        s.dirs.insert(PathBuf::from("/opt/rustmote-relay"));
        let existing = RelayState::new_bootstrap(
            base_clock(),
            "0.1.0",
            vec![rustmote_core::relay_lifecycle::ImagePin {
                service: "hbbs".into(),
                repo: "rustdesk/rustdesk-server".into(),
                tag: "1.1.11".into(),
                digest: DIGEST_V1.into(),
                pinned_at: base_clock(),
            }],
        );
        s.files.insert(
            state_path(&PathBuf::from("/opt/rustmote-relay")),
            existing.to_toml_string().unwrap().into_bytes(),
        );
    }

    let hub = FakeHub {
        tags: vec!["1.1.11".into()],
        tag_to_digest: HashMap::from([("1.1.11".into(), DIGEST_V1.into())]),
    };
    let lc = RelayLifecycle::new(&host)
        .with_clock(frozen(base_clock()))
        .with_health_check_interval(Duration::from_millis(0))
        .with_health_check_timeout(Duration::from_millis(50));

    let report = lc
        .bootstrap(&bootstrap_opts(), &client_with_hub(hub))
        .await
        .unwrap();

    assert!(report.already_installed);
    assert_eq!(report.state.images.len(), 1);
}

#[tokio::test]
async fn bootstrap_refuses_foreign_install_directory() {
    let host = MockHost::new(HostState::healthy_debian());
    // Install dir present, but no .rustmote-state.toml — not ours.
    host.state
        .lock()
        .unwrap()
        .dirs
        .insert(PathBuf::from("/opt/rustmote-relay"));

    let hub = FakeHub {
        tags: vec!["1.1.11".into()],
        tag_to_digest: HashMap::from([("1.1.11".into(), DIGEST_V1.into())]),
    };
    let lc = RelayLifecycle::new(&host)
        .with_health_check_interval(Duration::from_millis(0))
        .with_health_check_timeout(Duration::from_millis(50));

    let err = lc
        .bootstrap(&bootstrap_opts(), &client_with_hub(hub))
        .await
        .unwrap_err();
    assert!(matches!(err, RustmoteError::RelayForeignInstall(_)));
}

#[tokio::test]
async fn bootstrap_aborts_on_docker_compose_v1_only() {
    let mut init = HostState::healthy_debian();
    init.compose_v2_present = false;
    init.compose_v1_present = true;
    let host = MockHost::new(init);

    let hub = FakeHub {
        tags: vec!["1.1.11".into()],
        tag_to_digest: HashMap::from([("1.1.11".into(), DIGEST_V1.into())]),
    };
    let lc = RelayLifecycle::new(&host);
    let err = lc
        .bootstrap(&bootstrap_opts(), &client_with_hub(hub))
        .await
        .unwrap_err();
    assert!(matches!(err, RustmoteError::DockerComposeV1Detected));
}

#[tokio::test]
async fn update_happy_path_rewrites_pins_and_stamps_last_updated_at() {
    let host = MockHost::new(HostState::healthy_debian());
    seed_bootstrapped_host(&host, base_clock(), DIGEST_V1);

    let hub = FakeHub {
        tags: vec!["1.1.11".into(), "1.1.14".into()],
        tag_to_digest: HashMap::from([
            ("1.1.11".into(), DIGEST_V1.into()),
            ("1.1.14".into(), DIGEST_V2.into()),
        ]),
    };
    let update_time = base_clock() + chrono::Duration::hours(1);
    let lc = RelayLifecycle::new(&host)
        .with_clock(frozen(update_time))
        .with_health_check_interval(Duration::from_millis(0))
        .with_health_check_timeout(Duration::from_millis(50));

    let opts = UpdateOptions {
        install_path: PathBuf::from("/opt/rustmote-relay"),
        compose_template: compose_template(),
        services: vec!["hbbs".into(), "hbbr".into()],
        repo: "rustdesk/rustdesk-server".into(),
        assume_yes: true,
        is_tty: false,
        skip_backup: false,
        rustmote_version: "0.1.0".into(),
    };
    let report = lc.update(&opts, &client_with_hub(hub)).await.unwrap();

    assert!(report.changed);
    assert!(!report.rolled_back);
    assert_eq!(report.new_state.install.last_updated_at, Some(update_time));
    for p in &report.new_state.images {
        assert_eq!(p.tag, "1.1.14");
        assert_eq!(p.digest, DIGEST_V2);
    }
    // Backup dir was created under backups/.
    let expected_backup = backup_dir_for(&opts.install_path, update_time);
    assert!(host.state.lock().unwrap().dirs.contains(&expected_backup));
    // Persisted state file reflects the new pins.
    let state = host
        .read_file("/opt/rustmote-relay/.rustmote-state.toml")
        .unwrap();
    let parsed =
        RelayState::from_toml_str(std::str::from_utf8(&state).unwrap(), "state.toml").unwrap();
    assert_eq!(parsed.images[0].digest, DIGEST_V2);
}

#[tokio::test]
async fn update_rolls_back_on_health_check_failure() {
    let host = MockHost::new(HostState::healthy_debian());
    seed_bootstrapped_host(&host, base_clock(), DIGEST_V1);
    // The first `docker compose up -d` after pull swaps the open-ports
    // set to empty — simulating "new image started but hbbs/hbbr never
    // responded". The rollback's subsequent `up -d` must swap it back.
    {
        let mut s = host.state.lock().unwrap();
        s.pending_up_ports_swap = Some(std::collections::HashSet::new());
    }

    let hub = FakeHub {
        tags: vec!["1.1.11".into(), "1.1.14".into()],
        tag_to_digest: HashMap::from([
            ("1.1.11".into(), DIGEST_V1.into()),
            ("1.1.14".into(), DIGEST_V2.into()),
        ]),
    };
    let update_time = base_clock() + chrono::Duration::hours(1);
    let lc = RelayLifecycle::new(&host)
        .with_clock(frozen(update_time))
        .with_health_check_interval(Duration::from_millis(0))
        .with_health_check_timeout(Duration::from_millis(30));

    // Arrange for the rollback to restore health: re-seed the swap on
    // the next `up -d` call to re-open the ports. We do that by
    // intercepting via another pending_up_ports_swap set from the
    // background — simplest: pre-load a second swap that a second `up
    // -d` call (the rollback) picks up.
    //
    // Our mock only stores one swap at a time. Simulate a "sticky"
    // rollback by spawning a task that re-arms the swap after the
    // first `up -d` consumes it.
    let host_clone = host.clone();
    tokio::spawn(async move {
        // Give the orchestrator time to issue its first `up -d`.
        for _ in 0..100 {
            let open = {
                let s = host_clone.state.lock().unwrap();
                s.open_ports.clone()
            };
            if open.is_empty() {
                // Arm the ports to come back on the next `up -d`.
                let mut s = host_clone.state.lock().unwrap();
                let mut back = std::collections::HashSet::new();
                back.insert(21_116);
                back.insert(21_117);
                s.pending_up_ports_swap = Some(back);
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    let opts = UpdateOptions {
        install_path: PathBuf::from("/opt/rustmote-relay"),
        compose_template: compose_template(),
        services: vec!["hbbs".into(), "hbbr".into()],
        repo: "rustdesk/rustdesk-server".into(),
        assume_yes: true,
        is_tty: false,
        skip_backup: false,
        rustmote_version: "0.1.0".into(),
    };
    let err = lc.update(&opts, &client_with_hub(hub)).await.unwrap_err();
    assert!(matches!(err, RustmoteError::RelayHealthCheckFailed));

    // After rollback, the compose file on disk should be the v1-pinned
    // one (matches what was in the backup), and the state file should
    // reflect the v1 digest.
    let compose = String::from_utf8(
        host.read_file("/opt/rustmote-relay/docker-compose.yml")
            .unwrap(),
    )
    .unwrap();
    assert!(
        compose.contains(DIGEST_V1),
        "rollback should restore v1 digest: {compose}"
    );
    let state = host
        .read_file("/opt/rustmote-relay/.rustmote-state.toml")
        .unwrap();
    let parsed =
        RelayState::from_toml_str(std::str::from_utf8(&state).unwrap(), "state.toml").unwrap();
    assert_eq!(parsed.images[0].digest, DIGEST_V1);
}

#[tokio::test]
async fn update_refuses_when_non_tty_and_not_assume_yes() {
    let host = MockHost::new(HostState::healthy_debian());
    seed_bootstrapped_host(&host, base_clock(), DIGEST_V1);

    let hub = FakeHub {
        tags: vec!["1.1.14".into()],
        tag_to_digest: HashMap::from([("1.1.14".into(), DIGEST_V2.into())]),
    };
    let lc = RelayLifecycle::new(&host);

    let opts = UpdateOptions {
        install_path: PathBuf::from("/opt/rustmote-relay"),
        compose_template: compose_template(),
        services: vec!["hbbs".into(), "hbbr".into()],
        repo: "rustdesk/rustdesk-server".into(),
        assume_yes: false,
        is_tty: false,
        skip_backup: false,
        rustmote_version: "0.1.0".into(),
    };
    let err = lc.update(&opts, &client_with_hub(hub)).await.unwrap_err();
    assert!(matches!(err, RustmoteError::RelayUpdateNotConfirmed));
}

#[tokio::test]
async fn update_short_circuits_when_already_up_to_date() {
    let host = MockHost::new(HostState::healthy_debian());
    seed_bootstrapped_host(&host, base_clock(), DIGEST_V1);

    // Hub returns same digest as already installed.
    let hub = FakeHub {
        tags: vec!["1.1.11".into()],
        tag_to_digest: HashMap::from([("1.1.11".into(), DIGEST_V1.into())]),
    };
    let lc = RelayLifecycle::new(&host);
    let opts = UpdateOptions {
        install_path: PathBuf::from("/opt/rustmote-relay"),
        compose_template: compose_template(),
        services: vec!["hbbs".into(), "hbbr".into()],
        repo: "rustdesk/rustdesk-server".into(),
        assume_yes: true,
        is_tty: false,
        skip_backup: false,
        rustmote_version: "0.1.0".into(),
    };
    let report = lc.update(&opts, &client_with_hub(hub)).await.unwrap();
    assert!(!report.changed);
    assert!(report.backup_dir.is_none());
    assert!(report.gc_deleted.is_empty());
}

#[tokio::test]
async fn gc_removes_backups_older_than_seven_days_but_keeps_three_recent() {
    let host = MockHost::new(HostState::healthy_debian());
    seed_bootstrapped_host(&host, base_clock(), DIGEST_V1);

    // Seed five backups: one 30 days old, four within 7 days.
    let install = PathBuf::from("/opt/rustmote-relay");
    {
        let mut s = host.state.lock().unwrap();
        let ages = [30, 5, 4, 3, 2];
        for d in ages {
            let t = base_clock() - chrono::Duration::days(d);
            let p = backup_dir_for(&install, t);
            s.dirs.insert(p);
        }
    }

    let hub = FakeHub {
        tags: vec!["1.1.14".into()],
        tag_to_digest: HashMap::from([("1.1.14".into(), DIGEST_V2.into())]),
    };
    let update_time = base_clock() + chrono::Duration::hours(1);
    let lc = RelayLifecycle::new(&host)
        .with_clock(frozen(update_time))
        .with_health_check_interval(Duration::from_millis(0))
        .with_health_check_timeout(Duration::from_millis(50));

    let opts = UpdateOptions {
        install_path: install.clone(),
        compose_template: compose_template(),
        services: vec!["hbbs".into(), "hbbr".into()],
        repo: "rustdesk/rustdesk-server".into(),
        assume_yes: true,
        is_tty: false,
        skip_backup: false,
        rustmote_version: "0.1.0".into(),
    };
    let report = lc.update(&opts, &client_with_hub(hub)).await.unwrap();
    // The 30-day-old one is the only thing eligible: we seeded 5 dirs,
    // and after update a 6th one is created. Retention keeps the 3
    // newest regardless of age. Of the remaining 3 (the 30d + the 5d +
    // the 4d), only the 30d exceeds the cutoff.
    assert_eq!(
        report.gc_deleted.len(),
        1,
        "expected exactly one old backup to be pruned, got {:?}",
        report.gc_deleted
    );
    let pruned = &report.gc_deleted[0];
    assert!(
        pruned.to_string_lossy().contains("pre-update-"),
        "pruned entry should be a pre-update-* dir: {pruned:?}"
    );
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn seed_bootstrapped_host(host: &MockHost, at: DateTime<Utc>, digest: &str) {
    let install = PathBuf::from("/opt/rustmote-relay");
    let mut s = host.state.lock().unwrap();
    s.dirs.insert(install.clone());
    s.dirs.insert(data_path(&install));
    s.dirs.insert(backups_path(&install));

    let state = RelayState::new_bootstrap(
        at,
        "0.1.0",
        vec![
            rustmote_core::relay_lifecycle::ImagePin {
                service: "hbbs".into(),
                repo: "rustdesk/rustdesk-server".into(),
                tag: "1.1.11".into(),
                digest: digest.into(),
                pinned_at: at,
            },
            rustmote_core::relay_lifecycle::ImagePin {
                service: "hbbr".into(),
                repo: "rustdesk/rustdesk-server".into(),
                tag: "1.1.11".into(),
                digest: digest.into(),
                pinned_at: at,
            },
        ],
    );
    s.files.insert(
        state_path(&install),
        state.to_toml_string().unwrap().into_bytes(),
    );
    // A compose file with the installed digest pinned — matches what
    // bootstrap would have written.
    let compose = format!(
        "services:\n  hbbs:\n    image: rustdesk/rustdesk-server:1.1.11@{digest}\n  hbbr:\n    image: rustdesk/rustdesk-server:1.1.11@{digest}\n",
    );
    s.files.insert(compose_path(&install), compose.into_bytes());
    s.files
        .insert(env_path(&install), b"RUSTMOTE_RELAY=1\n".to_vec());
    s.files.insert(
        data_path(&install).join("id_ed25519.pub"),
        b"ssh-ed25519 AAAA... rustmote-relay\n".to_vec(),
    );
}
