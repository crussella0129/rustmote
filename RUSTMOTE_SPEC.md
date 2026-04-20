# Rustmote — Production Specification

**Target executor:** Claude Code
**Project owner:** Charles Russell (Thread & Signal LLC)
**Repo target:** `github.com/crussella0129/rustmote`
**License:** MIT OR Apache-2.0 (dual, matches Rust ecosystem norm)

---

## 0. Executive summary

Rustmote is a Rust-native remote-desktop jump-host orchestrator. It manages a registry of self-hosted RustDesk relay servers, discovers targets on local networks, establishes SSH tunnels to those relays, and launches the RustDesk viewer against the tunneled endpoint. It also manages the full lifecycle of the relay itself — bootstrap, update, rollback — from the client. It ships as a CLI today and a Tauri GUI later, both backed by a shared core library.

**What it is not:** A VNC/RDP protocol reimplementation. A replacement for RustDesk itself. An Electron app.

**What problem it solves:** The current workflow for "connect to a machine on my homelab through a self-hosted relay" is a pile of shell aliases, `ssh -L` incantations, remembered IPs, and hand-edited `docker-compose.yml` files on the relay host. Rustmote makes connection one command and relay maintenance three well-behaved commands, with secure-by-default credential handling throughout.

---

## 1. Workspace layout

Create a Cargo workspace exactly as follows. Do not deviate.

```
rustmote/
├── Cargo.toml                   # workspace root
├── README.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── .gitignore
├── .github/
│   └── workflows/
│       ├── ci.yml              # test + clippy + fmt on push
│       └── release.yml         # cross-platform binary builds on tag
├── crates/
│   ├── rustmote-core/          # library — all real logic lives here
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs       # TOML config load/save
│   │       ├── registry.rs     # RemoteServer registry
│   │       ├── target.rs       # Target type + discovery
│   │       ├── discovery.rs    # ARP + mDNS + ping sweep
│   │       ├── credentials.rs  # three-tier credential model
│   │       ├── session.rs      # SSH tunnel orchestration
│   │       ├── viewer.rs       # RustDesk viewer invocation
│   │       ├── registry_client.rs  # Docker Hub registry API client
│   │       ├── relay_lifecycle.rs  # bootstrap/update/rollback logic
│   │       └── error.rs        # thiserror-based error types
│   └── rustmote-cli/            # binary — clap-based CLI
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           └── commands/
│               ├── mod.rs
│               ├── server.rs
│               ├── target.rs
│               ├── connect.rs
│               ├── relay.rs
│               ├── config.rs
│               └── status.rs
├── docker/
│   └── relay/
│       ├── docker-compose.yml
│       ├── .env.example
│       └── README.md
└── docs/
    ├── ARCHITECTURE.md
    ├── SECURITY.md
    └── DEPLOYMENT.md
```

The `rustmote-gui` (Tauri) crate is explicitly **out of scope for v0.1**. Do not scaffold it. Do not add placeholders for it. It ships in v0.2 after the CLI has been dogfooded for at least two weeks.

---

## 2. Workspace `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = ["crates/rustmote-core", "crates/rustmote-cli"]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
authors = ["Charles Russell <your-email>"]
license = "MIT OR Apache-2.0"
repository = "https://github.com/crussella0129/rustmote"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
thiserror = "1"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive", "env"] }
russh = "0.45"
russh-keys = "0.45"
keyring = "3"
directories = "5"
mdns-sd = "0.11"
surge-ping = "0.8"
pnet = "0.35"
ipnet = "2"
rpassword = "7"
comfy-table = "7"
indicatif = "0.17"
dialoguer = "0.11"
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
chrono = { version = "0.4", features = ["serde"] }

[profile.release]
lto = "thin"
codegen-units = 1
strip = true
```

Pin these exact major versions. If a crate has had a breaking 1.x release between when this spec was written and when you execute it, check the current API rather than blindly bumping.

---

## 3. Core library — `rustmote-core`

### 3.1 Types

**`RemoteServer`** (registry entry):

```rust
pub struct RemoteServer {
    pub name: String,              // human-friendly name, unique primary key
    pub host: IpAddr,              // or hostname; IpAddr for v0.1
    pub ssh_port: u16,             // default 22
    pub ssh_user: String,          // user for SSH tunnel
    pub relay_port: u16,           // RustDesk hbbs port, default 21116
    pub relay_key: Option<String>, // RustDesk relay public key if set
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
}
```

**`Target`** (machine you connect to through the relay):

```rust
pub struct Target {
    pub id: String,                // RustDesk ID (9-10 digit) or friendly name
    pub ip: Option<IpAddr>,        // last known LAN IP if discovered
    pub label: Option<String>,     // user-assigned nickname
    pub via_server: Option<String>,// which RemoteServer to route through
    pub last_seen: Option<DateTime<Utc>>,
}
```

**`CredentialMode`**:

```rust
pub enum CredentialMode {
    Prompt,       // default, ask every time
    Keychain,     // OS keyring via `keyring` crate
    Unsafe,       // plaintext in config; requires explicit opt-in flag
}
```

### 3.2 Configuration

Config lives at the OS-appropriate location via the `directories` crate:

- Linux: `$XDG_CONFIG_HOME/rustmote/config.toml` (fallback `~/.config/rustmote/`)
- Windows: `%APPDATA%\rustmote\config.toml`
- macOS: `~/Library/Application Support/rustmote/config.toml`

Schema:

```toml
[general]
credential_mode = "prompt"   # prompt | keychain | unsafe
default_server = "zima-brain"
viewer_path = ""             # override path to rustdesk binary; empty = auto-detect

[[servers]]
name = "zima-brain"
host = "10.0.0.1"
ssh_port = 22
ssh_user = "charles"
relay_port = 21116
relay_key = ""

[[targets]]
id = "123456789"
label = "voron-controller"
via_server = "zima-brain"
```

Plaintext credentials (unsafe mode only) go in a **separate file** at `$CONFIG/rustmote/credentials.toml` with mode `0600` on Unix. Never in the main config. Creating this file requires the user to have already run `rustmote config set-mode unsafe --i-understand-this-is-insecure`.

### 3.3 Credential handling

Three functions in `credentials.rs`:

```rust
pub async fn get_password(server_name: &str, username: &str) -> Result<String>;
pub async fn set_password(server_name: &str, username: &str, password: &str) -> Result<()>;
pub async fn delete_password(server_name: &str, username: &str) -> Result<()>;
```

Dispatch by `CredentialMode`:

- **Prompt:** `rpassword::prompt_password()` every call. `set_password` is a no-op (returns Ok without storing).
- **Keychain:** Use the `keyring` crate. Service name: `"rustmote"`. Account format: `"{server_name}:{username}"`.
- **Unsafe:** Read/write `credentials.toml`. Refuse to operate if the file permissions are wider than `0600` on Unix. Log a warning on every access.

### 3.4 SSH session orchestration

Use `russh` (pure-Rust, no OpenSSL). The session flow:

1. Load `RemoteServer` from registry by name.
2. Acquire password for `ssh_user@server` via `credentials::get_password`.
3. Open SSH connection.
4. Establish local port forward: `localhost:<random-free-port>` → `localhost:relay_port` on the server.
5. Return a `Session` handle that owns the forwarding task.
6. On drop, cleanly tear down the forward.

**Prefer key-based auth.** Implement password auth as fallback only. Key path resolution: `~/.ssh/id_ed25519` → `~/.ssh/id_rsa` → config override. If a passphrase is required, prompt with `rpassword`.

### 3.5 Viewer invocation

`viewer.rs` locates the RustDesk binary:

- Linux: `which rustdesk` → `/usr/bin/rustdesk` → `/opt/rustdesk/rustdesk` → `flatpak run com.rustdesk.RustDesk`
- Windows: `%PROGRAMFILES%\RustDesk\rustdesk.exe` → registry query fallback
- macOS: `/Applications/RustDesk.app/Contents/MacOS/rustdesk`

Launch with args directing it to `127.0.0.1:<forwarded-port>` and the target ID. Exact CLI flags: check the RustDesk version installed — they change between 1.2.x and 1.3.x. Document the supported range in the README.

**This is the one place shelling out is acceptable.** Invoke only with validated args. Never pass user input directly — sanitize against shell metacharacters and validate the target ID matches `^[0-9]{9,10}$` or an explicit allowlist pattern for labels.

### 3.6 Discovery

`discovery.rs` implements three scan methods:

1. **mDNS/Bonjour sweep** via `mdns-sd` for services advertising `_ssh._tcp` and `_workstation._tcp`.
2. **ICMP ping sweep** via `surge-ping` across a configurable CIDR (default: auto-detect local subnet via `pnet`).
3. **ARP table read** via `pnet` for already-known MACs.

Return `Vec<DiscoveredHost>` with `{ ip, hostname, mac, is_known_server }`. The `is_known_server` flag is true if the IP matches any `RemoteServer.host` in the registry.

Discovery must complete in under 10 seconds on a `/24`. Run the three methods concurrently with `tokio::join!`.

### 3.7 Docker Hub registry client

`registry_client.rs` is a thin async client over Docker Hub's v2 registry API. Responsibilities:

- List tags for a given repo.
- Resolve a tag to its current manifest digest.
- Fetch the image manifest to verify digest format.
- Handle anonymous access for public images. Structure the API so optional auth (for private registries) can slot in later, but do not implement it in v0.1.
- Rate-limit-aware: cache responses at `$CACHE/rustmote/docker-hub-cache.toml` with a configurable TTL (default 1 hour).

Use `reqwest` with `rustls-tls` (no OpenSSL dependency anywhere in the stack).

### 3.8 Relay lifecycle logic

`relay_lifecycle.rs` contains the bootstrap/check-updates/update/rollback state machines. This module executes commands on a remote host via the `session` module's SSH channel API — **never** shells out to the local `ssh` binary and never scp's temporary scripts. All remote command strings are built in Rust, validated against allowlists, and executed via `channel.exec()`.

Full behavior specified in §5.1 below.

### 3.9 Error handling

Use `thiserror` in `rustmote-core` for library errors. Use `anyhow` in `rustmote-cli` for top-level error context. Do not let `unwrap()` or `expect()` ship in non-test code except for genuinely-impossible invariants (and comment why).

Error variants at minimum:

```rust
#[derive(thiserror::Error, Debug)]
pub enum RustmoteError {
    #[error("config file not found at {0}")]
    ConfigNotFound(PathBuf),
    #[error("server '{0}' not in registry")]
    UnknownServer(String),
    #[error("ssh connection failed: {0}")]
    SshConnection(#[from] russh::Error),
    #[error("credential access failed: {0}")]
    Credential(#[from] keyring::Error),
    #[error("viewer binary not found; install RustDesk or set viewer_path")]
    ViewerNotFound,
    #[error("unsafe mode requires explicit acknowledgment flag")]
    UnsafeModeNotAcknowledged,
    #[error("credentials file has insecure permissions: {0:o}")]
    InsecureCredentialsFile(u32),
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
```

---

## 4. CLI — `rustmote-cli`

### 4.1 Command surface

Implement exactly these subcommands with `clap` derive API:

```
rustmote server add <n> --host <ip> [--user <username>] [--ssh-port <port>] [--relay-port <port>]
rustmote server list
rustmote server remove <n>
rustmote server show <n>

rustmote target scan [--cidr <cidr>] [--timeout <secs>]
rustmote target list
rustmote target add <id> [--label <label>] [--via <server>]
rustmote target remove <id>

rustmote connect <target> [--via <server>] [--user <username>]
rustmote status

rustmote relay bootstrap <server-name> [--os <debian|ubuntu|arch|auto>] [--compose-path <path>]
rustmote relay check-updates <server-name> [--json]
rustmote relay update <server-name> [--yes] [--skip-backup]
rustmote relay status <server-name>
rustmote relay logs <server-name> [--follow] [--tail <n>]
rustmote relay stop <server-name>
rustmote relay start <server-name>
rustmote relay restart <server-name>

rustmote config show
rustmote config set-mode <mode> [--i-understand-this-is-insecure]
rustmote config path
```

### 4.2 Output conventions

- Tables via `comfy-table` with a minimal borderless style by default.
- Progress for scans via `indicatif`.
- Interactive prompts (`dialoguer`) only when a flag is missing AND stdin is a TTY. If not a TTY, fail with a clear error telling the user which flag to pass.
- JSON output via `--json` on every list/show/scan/check command. Structured logs go to stderr; data goes to stdout. This matters for scripting.

### 4.3 Logging

`tracing_subscriber` with `EnvFilter`. Default level `warn`. `-v` = info, `-vv` = debug, `-vvv` = trace. Never log passwords or raw key material.

---

## 5. Docker relay deployment

### 5.0 The shipped compose template

`docker/relay/docker-compose.yml`:

```yaml
services:
  hbbs:
    image: rustdesk/rustdesk-server:1.1.11
    container_name: rustmote-hbbs
    command: hbbs -r hbbr:21117 -k _
    ports:
      - "127.0.0.1:21115:21115"
      - "127.0.0.1:21116:21116"
      - "127.0.0.1:21116:21116/udp"
      - "127.0.0.1:21118:21118"
    volumes:
      - ./data:/root
    restart: unless-stopped
    depends_on:
      - hbbr

  hbbr:
    image: rustdesk/rustdesk-server:1.1.11
    container_name: rustmote-hbbr
    command: hbbr -k _
    ports:
      - "127.0.0.1:21117:21117"
      - "127.0.0.1:21119:21119"
    volumes:
      - ./data:/root
    restart: unless-stopped
```

**Critical:** Ports bind to `127.0.0.1` only. The relay is reached over WireGuard or SSH tunnel, never the open LAN. Document this in `docker/relay/README.md` with a ten-line setup guide.

The `-k _` flag makes RustDesk generate a keypair on first run. Public key ends up in `./data/id_ed25519.pub`. The `bootstrap` command below automates copying this back into the local registry.

The template ships with a tag-only pin (`1.1.11`). The `bootstrap` command resolves and rewrites this to include the digest at install time.

---

### 5.1 Relay lifecycle commands

Three commands manage the lifecycle of a self-hosted RustDesk relay from the client machine. These operate over SSH against a `RemoteServer` already in the registry.

**No auto-update mechanism is shipped.** Updates are explicitly user-initiated. This is a deliberate design choice for a single-operator tool where uptime matters more than having the newest bits. Auto-update creates a failure mode where a broken upstream release locks you out of your own infrastructure at an unpredictable time.

#### 5.1.1 `relay bootstrap`

One-shot install on a fresh server. Idempotent — safe to re-run; exits early with a clear message if the relay is already installed.

Steps, in order:

1. SSH to the server using the credentials flow from §3.3.
2. Detect OS via `/etc/os-release`. If `--os` was passed explicitly, honor it. If auto-detection fails and `--os` is not set, abort with a clear message listing supported OSes.
3. Verify Docker Engine and the Compose v2 plugin are installed:
    - If missing on Debian/Ubuntu: run the official `get.docker.com` script **after showing it to the user and prompting for confirmation**.
    - If missing on Arch: `pacman -S docker docker-compose` with confirmation.
    - If missing and unsupported OS: abort with a message telling the user to install Docker manually and re-run.
    - If `docker-compose` (v1) is detected but not `docker compose` (v2), abort with `DockerComposeV1Detected` and a migration hint.
4. Create `/opt/rustmote-relay/` on the server (configurable via `--compose-path`).
5. Resolve current digests for `rustdesk/rustdesk-server:1.1.11` (or whatever tag is pinned in the shipped template) from Docker Hub via `registry_client`.
6. Write `docker-compose.yml` with both tag and digest pinned:
    ```yaml
    image: rustdesk/rustdesk-server:1.1.11@sha256:<resolved-digest>
    ```
7. Write `.env` with generated values.
8. `docker compose up -d` from the install directory.
9. Poll the relay's health endpoint for up to 30 seconds (TCP connect to `127.0.0.1:21116` and `127.0.0.1:21117`). On failure: dump logs to stderr and abort. Do not leave a half-installed relay running silently.
10. Read the generated `data/id_ed25519.pub`, update the `RemoteServer.relay_key` field in the local registry.
11. Print a summary: install path, pinned image tag+digest, relay public key, next steps.

**Required preconditions:**
- Server must already be registered via `rustmote server add`.
- SSH connection must succeed before any remote writes happen.

**State on the server after bootstrap:**

```
/opt/rustmote-relay/
├── docker-compose.yml
├── .env
├── .rustmote-state.toml      # pinned image digests, install timestamp, rustmote version
├── data/
│   ├── id_ed25519
│   └── id_ed25519.pub
└── backups/                   # created but empty
```

`.rustmote-state.toml` is the source of truth for what versions are currently deployed. Schema:

```toml
[install]
bootstrapped_at = "2026-04-18T12:34:56Z"
bootstrapped_by_rustmote_version = "0.1.0"

[[images]]
service = "hbbs"
repo = "rustdesk/rustdesk-server"
tag = "1.1.11"
digest = "sha256:abc123..."
pinned_at = "2026-04-18T12:34:56Z"

[[images]]
service = "hbbr"
repo = "rustdesk/rustdesk-server"
tag = "1.1.11"
digest = "sha256:abc123..."
pinned_at = "2026-04-18T12:34:56Z"
```

#### 5.1.2 `relay check-updates`

Read-only. Safe to run unattended (including from cron if the user wants visibility without taking action). Does not modify any state on the server.

Steps:

1. SSH to the server.
2. Read `/opt/rustmote-relay/.rustmote-state.toml` to learn current pinned tags and digests.
3. Query Docker Hub's v2 registry API for the latest tag matching the pinned repo's versioning pattern (default: latest semver tag).
4. For each image, resolve the digest of the latest tag.
5. Print a table via `comfy-table`:
    ```
    SERVICE  REPO                        CURRENT     LATEST      STATUS
    hbbs     rustdesk/rustdesk-server    1.1.11      1.1.14      update available
    hbbr     rustdesk/rustdesk-server    1.1.11      1.1.14      update available
    ```
6. If `--json`, emit structured output to stdout. Logging goes to stderr per §4.2.
7. Exit 0 regardless of whether updates exist. Reserve non-zero exit for actual errors (SSH failure, Docker Hub unreachable, state file missing).

Rate-limit Docker Hub API calls — the `registry_client` cache (§3.7) handles this.

#### 5.1.3 `relay update`

Deliberate, interactive, rollback-able.

Steps:

1. Run the same query logic as `check-updates`.
2. If no updates are available, print "already up to date" and exit 0.
3. Display the update table.
4. Unless `--yes` was passed, prompt interactively: "Proceed with update? [y/N]". Default is No. If stdin is not a TTY and `--yes` was not passed, abort with an error — **never auto-proceed in non-interactive mode**.
5. Create a snapshot in `/opt/rustmote-relay/backups/pre-update-<ISO-timestamp>/`:
    - Copy of current `docker-compose.yml`
    - Copy of current `.rustmote-state.toml`
    - Current `docker compose config` output
   Skip this step only if `--skip-backup` is explicitly passed.
6. Rewrite `docker-compose.yml` with the new tags and digests.
7. Update `.rustmote-state.toml` with the new pins.
8. `docker compose pull` — fetch new images.
9. `docker compose up -d` — recreate containers.
10. Health-check loop: poll the relay for 30 seconds, checking hbbs on `127.0.0.1:21116` and hbbr on `127.0.0.1:21117`.
11. **On health-check failure:** automatically restore the backup, `docker compose up -d` with old images, verify the rollback is healthy, surface the failure to the user with the log output from the failed new version. Exit with `RelayHealthCheckFailed`.
12. **On health-check success:** print success summary with the new pinned digests.
13. Garbage-collect backups older than 7 days (keep at minimum the three most recent regardless of age).

**The rollback is the feature.** Without automatic rollback on health failure, this command is more dangerous than manual `docker compose pull`. With rollback, it's safer.

#### 5.1.4 `relay status`

Diagnostic command. Shows:

- Install path on server
- Currently pinned image tags and digests (from `.rustmote-state.toml`)
- Container status (`docker compose ps` parsed output)
- Uptime of each container
- Relay public key
- Last update timestamp
- Disk usage of the install directory

#### 5.1.5 `relay logs` / `start` / `stop` / `restart`

Thin wrappers over `docker compose logs`/`start`/`stop`/`restart` executed over SSH. These exist because the whole point of Rustmote is to avoid hand-SSHing into the relay for routine operations.

`logs --follow` streams output from the remote `docker compose logs -f` over the SSH channel until the user hits Ctrl-C. Handle SIGINT cleanly — close the channel, don't leave orphan processes on the server.

#### 5.1.6 Implementation notes

- All lifecycle commands run remote SSH commands using the `russh` channel API. Do not shell out to the local `ssh` binary. Do not write a temporary script and scp it over — build the command strings in Rust, validated against allowlists, and execute via `channel.exec()`.
- Command strings passed to `channel.exec()` must never interpolate unvalidated user input. Server names, paths, and tags go through the same regex allowlists as §6.
- `docker compose` version detection: some systems have `docker-compose` (v1, deprecated) and some have `docker compose` (v2 plugin). Detect which is present. Prefer v2. Refuse to proceed with v1 — document the migration path in the error message.

---

## 6. Security requirements

These are non-negotiable. Failing any of these is a release blocker.

1. **No credentials in process arguments.** Passing a password as `rustmote connect --password foo` is forbidden. Passwords come from keyring, prompt, or credentials file — never argv.
2. **Unsafe mode requires `--i-understand-this-is-insecure` the first time it is enabled.** Log a warning on every subsequent invocation while unsafe mode is active.
3. **Credentials file permission check.** On Unix, refuse to read `credentials.toml` if mode is wider than `0600`. On Windows, verify ACLs restrict to the current user.
4. **Validate all user input used in shelled-out or remote-exec'd commands** against strict regex allowlists. Target IDs: `^[0-9]{9,10}$`. Server names: `^[a-zA-Z0-9_-]{1,64}$`. Paths: canonicalized and checked against a base-path prefix.
5. **No `unsafe` Rust blocks** without a comment explaining why and a link to the invariant being upheld. Target: zero `unsafe` in v0.1.
6. **Clippy clean at `--deny warnings`** including `clippy::pedantic` (allow-list specific lints in `lib.rs` with rationale).
7. **SSH host key verification is mandatory.** First-use prompt with fingerprint display. Store fingerprints in `known_hosts.toml` alongside the main config. Refuse to connect on fingerprint mismatch with a clear warning.
8. **Never auto-update.** No timer, cron, systemd unit, or on-startup check that modifies relay state. `check-updates` is read-only and safe to automate; `update` is never automated by Rustmote itself.
9. **Always pin by digest, not just by tag.** Tag-only pins allow upstream tag repointing attacks. The `bootstrap` and `update` commands must always resolve and write digests.
10. **Rollback on health-check failure is mandatory** in `relay update`, not optional. The `--skip-backup` flag skips the snapshot but does not disable rollback attempts against whatever state is recoverable.
11. **Confirm before installing Docker.** The install script for Docker runs as root on the remote. The user must see what's about to execute and type `y` to proceed.
12. **Refuse to bootstrap over an existing non-Rustmote installation.** If `/opt/rustmote-relay/` exists but has no `.rustmote-state.toml`, abort rather than overwrite.

---

## 7. Testing requirements

### 7.1 Unit tests

Every module in `rustmote-core` gets a `#[cfg(test)] mod tests` block. Target >70% line coverage for the core crate by v0.1. Mock the SSH layer with a trait abstraction so session logic and relay lifecycle logic are testable without a real server.

### 7.2 Integration tests

`crates/rustmote-core/tests/`:

- `config_roundtrip.rs` — write, read, verify equality.
- `credential_modes.rs` — exercise all three modes with a mock keyring.
- `discovery_localhost.rs` — scan `127.0.0.0/29`, verify localhost appears.
- `registry_client_cache.rs` — verify TTL-based caching of Docker Hub responses.
- `relay_lifecycle_mock.rs` — bootstrap → check-updates → update → status against a mock SSH transport.
- `relay_rollback.rs` — simulate a health-check failure after `update`, assert the snapshot is restored and containers are running the old digest. Gate on `RUSTMOTE_INTEGRATION_DOCKER=1` env var so it only runs when Docker is available locally.

### 7.3 CLI smoke tests

`crates/rustmote-cli/tests/cli.rs` using `assert_cmd` + `predicates`:

- `rustmote --help` exits 0 and mentions each subcommand group (including `relay`).
- `rustmote server list` on an empty config returns empty table, exit 0.
- `rustmote config path` prints the expected path for the current OS.
- `rustmote relay check-updates nonexistent-server` returns `UnknownServer` error, non-zero exit.

### 7.4 CI matrix

`.github/workflows/ci.yml` runs on:

- ubuntu-latest (stable, beta, MSRV 1.85)
- windows-latest (stable)
- macos-latest (stable)

Jobs: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-features`, `cargo doc --no-deps`.

---

## 8. Documentation deliverables

- `README.md` — quickstart in under 100 lines, aimed at someone who has never used RustDesk. Must cover: install Rustmote, add a server, `relay bootstrap`, connect to a target.
- `docs/ARCHITECTURE.md` — the data flow through core → cli, the three credential modes, the SSH-tunnel-to-relay model, the relay lifecycle state machine. One mermaid diagram per major flow.
- `docs/SECURITY.md` — threat model: what Rustmote protects against (network sniffing, credential exfiltration from disk, tag-repointing attacks via digest pinning), what it does not (compromised relay host, supply-chain attacks on RustDesk itself, physical access to the workstation, malicious local user with sudo).
- `docs/DEPLOYMENT.md` — ZimaBoard relay setup walkthrough with exact commands, covering `bootstrap` through first successful `connect`.

---

## 9. Release checklist for v0.1.0

Before tagging:

- [ ] All CI jobs green on the target matrix
- [ ] `cargo deny check` clean (no security advisories, no license violations)
- [ ] Manually tested end-to-end on Linux → Linux, Windows → Linux, Linux → Windows
- [ ] `relay bootstrap` tested on a fresh Debian, Ubuntu, and Arch server
- [ ] `relay update` rollback path manually triggered and verified (point the compose file at a bad image to force health-check failure)
- [ ] README quickstart works verbatim on a fresh machine
- [ ] `docker compose up` in `docker/relay/` produces a working relay
- [ ] `cargo publish --dry-run` succeeds for both crates
- [ ] Changelog entry in `CHANGELOG.md` following Keep-a-Changelog format

---

## 10. Explicitly out of scope for v0.1

Do not build these. They are v0.2 or later:

- Tauri GUI
- Embedded noVNC viewer
- Multi-hop SSH (jump host through jump host)
- Web dashboard
- Mobile clients
- Windows service / Linux daemon mode
- Telemetry of any kind
- Auto-update of anything (relay or CLI)
- Self-update of the Rustmote CLI itself
- Multi-relay orchestration (updating N relays in sequence)
- Canary deployments / staged rollouts
- Integration with external secret stores (Vault, SOPS)
- Image signature verification via cosign/sigstore (candidate for v0.3)

If you find yourself building any of these, stop and finish the v0.1 surface first.

---

## 11. Build order

Execute in this order. Do not parallelize phases.

1. Workspace scaffold, CI, licenses, README skeleton.
2. `rustmote-core::config` + `registry` + tests.
3. `rustmote-core::credentials` with all three modes + tests.
4. `rustmote-core::session` (SSH tunnel) + tests with mock transport.
5. `rustmote-core::viewer` (binary detection + invocation) + tests.
6. `rustmote-core::discovery` + tests.
7. `rustmote-cli::server` subcommands.
8. `rustmote-cli::target` subcommands.
9. `rustmote-cli::connect` — the payoff command for the session layer.
10. `rustmote-cli::config` + `status`.
11. `rustmote-core::registry_client` (Docker Hub API) + tests.
12. `rustmote-core::relay_lifecycle` (bootstrap, update, rollback) + tests.
13. `rustmote-cli::relay` subcommands.
14. Docker compose template for the relay.
15. Documentation pass.
16. Release checklist.

---

## 12. Style rules

- `cargo fmt` default settings.
- Clippy clean at `-D warnings` with `clippy::pedantic` enabled.
- Public API items get doc comments with at least one example.
- No `println!` in library code. Use `tracing`.
- No `.unwrap()` or `.expect()` outside of tests except for genuinely infallible invariants with a `// SAFETY:` or `// INVARIANT:` comment.
- Prefer `&str` over `String` in function signatures unless ownership is required.
- `#[must_use]` on builders and anything returning a `Result` wrapper type.

---

## 13. Questions to flag rather than guess

If any of the following come up during implementation, stop and surface them to the project owner rather than picking a default:

- Behavior when a target's `via_server` is not in the registry.
- Whether to support IPv6 in v0.1 (default: no, log and skip IPv6 addresses in discovery).
- How to handle RustDesk viewer version mismatches (default: detect version, warn if outside tested range, proceed).
- Whether to offer a `rustmote init` wizard for first-run setup.
- Whether `relay bootstrap` should refuse to run on a server that already has Docker containers running under other names (conservative: warn and proceed; paranoid: abort and require `--force`).
- How to handle Docker Hub being unreachable during `bootstrap` (fall back to tag-only pinning with a loud warning, or abort?).

---

**End of specification.** Execute in the order given in §11. Keep the v0.1 surface tight; ship, dogfood, iterate.
