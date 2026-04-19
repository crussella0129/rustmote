# Environment — Rustmote

**Captured:** 2026-04-19 01:14:31
**Last Updated:** 2026-04-19

## Development Machine

- **OS:** Ubuntu Linux (kernel 6.8.0-107-generic), x86_64
- **Shell:** bash
- **User repos dir:** `~/repos` (see user memory `repos_setup.md`)
- **GitHub protocol:** SSH (user `crussella0129`, `gh` CLI authenticated via keyring)

## Runtime Versions

| Tool | Version |
|------|---------|
| rustc | 1.95.0 (stable) — well above MSRV 1.75 |
| cargo | 1.95.0 |
| docker | 29.4.0 |
| docker compose | v5.1.3 (v2 plugin, required by spec) |
| gh | authenticated |

## Package State

- `Cargo.lock` will live at workspace root once Phase 1 scaffold is committed.
- No relay deployed yet; `docker/relay/docker-compose.yml` ships in Phase 14.

## Environment Variables

| Variable | Purpose |
|----------|---------|
| `RUST_LOG` | Controls `tracing_subscriber` EnvFilter level (default `warn`; `-v`/`-vv`/`-vvv` flags on CLI override). |
| `XDG_CONFIG_HOME` | Linux config path root; rustmote config at `$XDG_CONFIG_HOME/rustmote/config.toml`. |
| `RUSTMOTE_INTEGRATION_DOCKER` | Set to `1` to enable the `relay_rollback.rs` integration test (requires local Docker). |

## Target Platforms

- [x] Linux (primary — Debian/Ubuntu/Arch for relay bootstrap)
- [x] macOS (client-side only)
- [x] Windows (client-side only)
- [x] Docker (relay host deployment via `docker compose` v2)
- [ ] iOS
- [ ] Android
- [ ] Web

## CI Matrix (per RUSTMOTE_SPEC §7.4)

- ubuntu-latest: stable, beta, MSRV 1.75
- windows-latest: stable
- macos-latest: stable
- Jobs: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-features`, `cargo doc --no-deps`.
