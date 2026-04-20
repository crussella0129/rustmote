# Tasks — Rustmote

**Last Updated:** 2026-04-20 (Entry #14)

## Legend

- `[ ]` proposed/accepted (not started)
- `[~]` active (in progress)
- `[!:reason]` blocked (reason required)
- `[x]` completed (immutable; cite log entry)

State transitions are forward-only (or to blocked). Completed tasks are not re-opened —
file a new task instead.

Task IDs 001–016 mirror the 16 phases of RUSTMOTE_SPEC §11 "Build order" verbatim;
§11 mandates **do not parallelize phases**. See DECISION-001.

## Current Sprint

- [x] TASK-001 | TYPE: chore | SCOPE: medium | OWNER: agent — completed log Entry #2
  - Phase 1 — Workspace scaffold, CI, licenses, README skeleton. Create Cargo workspace layout per spec §1 (root `Cargo.toml`, `crates/rustmote-core`, `crates/rustmote-cli`, `docker/relay/`, `docs/`). Add dual MIT + Apache-2.0 licenses, `.gitignore`, empty README, `.github/workflows/{ci.yml,release.yml}`.

- [x] TASK-002 | TYPE: feature | SCOPE: medium | OWNER: agent — completed log Entry #4
  - Phase 2 — `rustmote-core::config` + `registry` + tests. Implement `RemoteServer` and `Target` structs (§3.1), TOML config load/save at OS-appropriate path via `directories` (§3.2), and registry CRUD. Unit + integration test (`config_roundtrip.rs`).

- [x] TASK-003 | TYPE: feature | SCOPE: medium | OWNER: agent — completed log Entry #5
  - Phase 3 — `rustmote-core::credentials` with all three modes + tests. Implement `CredentialMode::{Prompt, Keychain, Unsafe}` dispatch (§3.3), `0600` permission enforcement on `credentials.toml`, refusal to read unsafe file without explicit ack. Integration test `credential_modes.rs` with mock keyring.

- [x] TASK-004 | TYPE: feature | SCOPE: large | OWNER: agent — completed log Entry #6
  - Phase 4 — `rustmote-core::session` (SSH tunnel) + tests with mock transport. `russh`-based session (§3.4), local port forward to relay, key-first / password-fallback auth, mandatory SSH host-key TOFU verification with `known_hosts.toml` (spec §6.7). Abstract transport behind a trait so relay_lifecycle tests can mock it.

- [x] TASK-005 | TYPE: feature | SCOPE: small | OWNER: agent — completed log Entry #7
  - Phase 5 — `rustmote-core::viewer` (binary detection + invocation) + tests. Per-OS RustDesk lookup per §3.5, strict target-ID regex `^[0-9]{9,10}$`, no raw user input in argv. This is the only sanctioned shell-out path in the codebase.

- [x] TASK-006 | TYPE: feature | SCOPE: medium | OWNER: agent — completed log Entry #8
  - Phase 6 — `rustmote-core::discovery` + tests. Concurrent mDNS + ICMP ping sweep + ARP read (§3.6) via `tokio::join!`; must complete a /24 in <10s. Integration test `discovery_localhost.rs`.

- [x] TASK-007 | TYPE: feature | SCOPE: medium | OWNER: agent — completed log Entry #9
  - Phase 7 — `rustmote-cli::server` subcommands (`add`/`list`/`remove`/`show`). Clap derive API, `comfy-table` output, `--json` flag per §4.2, `dialoguer` prompts only when flag missing AND stdin is a TTY.

- [x] TASK-008 | TYPE: feature | SCOPE: medium | OWNER: agent — completed log Entry #10
  - Phase 8 — `rustmote-cli::target` subcommands (`scan`/`list`/`add`/`remove`). `indicatif` progress for scans.

- [x] TASK-009 | TYPE: feature | SCOPE: medium | OWNER: agent — completed log Entry #11
  - Phase 9 — `rustmote-cli::connect` — the payoff command wiring session → viewer.

- [x] TASK-010 | TYPE: feature | SCOPE: small | OWNER: agent — completed log Entry #12
  - Phase 10 — `rustmote-cli::config` and `rustmote-cli::status`. `config set-mode unsafe --i-understand-this-is-insecure` gate required.

- [x] TASK-011 | TYPE: feature | SCOPE: medium | OWNER: agent — completed log Entry #13
  - Phase 11 — `rustmote-core::registry_client` (Docker Hub API) + tests. Anonymous v2 registry access, tag listing, manifest digest resolution, TTL-cached responses at `$CACHE/rustmote/docker-hub-cache.toml`. Integration test `registry_client_cache.rs`.

- [x] TASK-012 | TYPE: feature | SCOPE: large | OWNER: agent — completed log Entry #14
  - Phase 12 — `rustmote-core::relay_lifecycle` (bootstrap, update, rollback) + tests. All commands executed via `russh` channel API with allowlisted args — never shell out, never scp temp scripts. Implement `.rustmote-state.toml` schema (§5.1.1), pre-update snapshots, auto-rollback on health-check failure, 7-day backup GC. Integration tests `relay_lifecycle_mock.rs` and gated `relay_rollback.rs` (`RUSTMOTE_INTEGRATION_DOCKER=1`).

- [ ] TASK-013 | TYPE: feature | SCOPE: medium | OWNER: agent
  - Phase 13 — `rustmote-cli::relay` subcommands (`bootstrap`/`check-updates`/`update`/`status`/`logs`/`start`/`stop`/`restart`). `relay update` must never auto-proceed when non-TTY without `--yes`.

- [ ] TASK-014 | TYPE: chore | SCOPE: small | OWNER: agent
  - Phase 14 — Docker compose template for the relay. `docker/relay/docker-compose.yml` per §5.0 with `127.0.0.1`-only port bindings, tag `1.1.11`, `-k _` for keypair gen, matching `.env.example` and 10-line README.

- [ ] TASK-015 | TYPE: docs | SCOPE: medium | OWNER: agent
  - Phase 15 — Documentation pass. `README.md` (<100 line quickstart), `docs/ARCHITECTURE.md` (data flow + mermaid per flow), `docs/SECURITY.md` (threat model — what we protect against vs not), `docs/DEPLOYMENT.md` (ZimaBoard walkthrough).

- [ ] TASK-016 | TYPE: chore | SCOPE: medium | OWNER: agent
  - Phase 16 — Release checklist for v0.1.0 (spec §9). CI green on matrix, `cargo deny check` clean, manual end-to-end verification (Linux→Linux, Windows→Linux, Linux→Windows), `relay bootstrap` on fresh Debian/Ubuntu/Arch, rollback path manually triggered, `cargo publish --dry-run` both crates, `CHANGELOG.md` entry.

## Backlog

- [ ] TASK-017 | TYPE: research | SCOPE: small | OWNER: human
  - Resolve spec §13 open questions as they surface during implementation: behavior when `via_server` missing from registry; IPv6 in v0.1 (default: no); RustDesk viewer version mismatch handling; whether to add a `rustmote init` wizard; `bootstrap` behavior when other Docker containers exist; Docker Hub unreachable during `bootstrap` (abort vs tag-only with warning). Flag to owner rather than guessing per spec.

- [x] TASK-018 | TYPE: chore | SCOPE: small | OWNER: agent — completed log Entry #3
  - Vendor `RUSTMOTE_SPEC.md` into repo root so sessions on other machines don't depend on `~/Downloads/`. Update `GECK/LLM_init.md` and `README.md` references to the in-repo path.

## Completed (Recent)

- [x] TASK-001 — Phase 1 workspace scaffold (log Entry #2, 2026-04-19)
- [x] TASK-018 — Vendored RUSTMOTE_SPEC.md into repo (log Entry #3, 2026-04-19)
- [x] TASK-002 — Phase 2 config + registry + tests (log Entry #4, 2026-04-19)
- [x] TASK-003 — Phase 3 credentials dispatch + 0600 enforcement + tests (log Entry #5, 2026-04-19)
- [x] TASK-004 — Phase 4 SSH session + host-key TOFU + mock transport trait (log Entry #6, 2026-04-19)
- [x] TASK-005 — Phase 5 viewer detection + TargetId validation + Command builder (log Entry #7, 2026-04-19)
- [x] TASK-006 — Phase 6 concurrent LAN discovery (mDNS + ICMP + ARP) (log Entry #8, 2026-04-20)
- [x] TASK-007 — Phase 7 CLI server subcommands + spec §7.3 smoke tests (log Entry #9, 2026-04-20)
- [x] TASK-008 — Phase 8 CLI target subcommands + indicatif scan spinner + §3.5 reuse (log Entry #10, 2026-04-20)
- [x] TASK-009 — Phase 9 CLI connect command wiring session → viewer (log Entry #11, 2026-04-20)
- [x] TASK-010 — Phase 10 CLI config show/set-mode + status with §6.2 gate (log Entry #12, 2026-04-20)
- [x] TASK-011 — Phase 11 registry_client Docker Hub v2 + TTL cache + hermetic tests (log Entry #13, 2026-04-20)
- [x] TASK-012 — Phase 12 relay_lifecycle bootstrap/update/rollback + strict-panic MockHost tests (log Entry #14, 2026-04-20)
