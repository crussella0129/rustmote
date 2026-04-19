# Project: Rustmote

**Repository:** github.com/crussella0129/rustmote
**Local Path:** ~/repos/rustmote
**Created:** 2026-04-19
**GECK Protocol:** v1.3
**Context Budget:** large

## Goal

Rust-native remote-desktop jump-host orchestrator that manages self-hosted RustDesk relays, discovers LAN targets, tunnels SSH to relay endpoints, and launches the RustDesk viewer.

Ships as a CLI in v0.1 backed by a shared core library (`rustmote-core`). A Tauri GUI crate (`rustmote-gui`) is explicitly deferred to v0.2.

## Success Criteria

- [ ] Single-binary CLI that can add a server, bootstrap a RustDesk relay, and connect to a target in one command
- [ ] Relay bootstrap/update/rollback is fully automated, idempotent, and auto-rolls-back on health-check failure
- [ ] Credentials handled via prompt / OS keyring / opt-in unsafe file (0600 enforced); never in argv
- [ ] All container images pinned by digest; SSH host keys verified on first use with TOFU fingerprint store
- [ ] Zero unsafe Rust; clippy pedantic clean at -D warnings; >70% line coverage in rustmote-core
- [ ] CI green on ubuntu-latest / windows-latest / macos-latest for stable, beta, and MSRV 1.75
- [ ] Cargo workspace with rustmote-core library and rustmote-cli binary; Tauri GUI deferred to v0.2

## Constraints

- **Languages:** Rust (edition 2021, MSRV 1.75)
- **Frameworks:** tokio, clap v4, russh 0.45, reqwest (rustls-tls), keyring 3, mdns-sd, pnet, tracing, thiserror, anyhow
- **Must use:** `tracing` for logs (no `println!` in library code), `thiserror` in core / `anyhow` in cli, `#[must_use]` on builders and Result wrappers, `&str` over `String` in signatures unless ownership is required, `rustls-tls` (no OpenSSL anywhere in the stack), digest pinning for all container images, SSH host-key TOFU verification
- **Must avoid:** `unsafe` blocks (target zero in v0.1), `.unwrap()`/`.expect()` outside tests, credentials in argv, shelling out to local `ssh`/`docker` (use `russh` channel API), tag-only image pins, auto-update of anything, un-validated user input in remote-exec command strings
- **Target platforms:** linux (primary), macos, windows

## Context

Authoritative spec lives at `~/Downloads/RUSTMOTE_SPEC.md` (13 sections). §11 of the spec prescribes a strict 16-phase build order — do not parallelize phases. §10 enumerates what is out of scope for v0.1 (Tauri GUI, noVNC, multi-hop SSH, auto-update, telemetry, etc.).

Owner: Charles Russell (Thread & Signal LLC). License: MIT OR Apache-2.0 dual.

## Initial Task

Execute §11 build order, beginning with Phase 1: workspace scaffold, CI, licenses, README skeleton. See `tasks.md` for the full phased task list (TASK-001 through TASK-016).
