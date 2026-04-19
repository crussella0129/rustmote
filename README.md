# Rustmote

> Rust-native remote-desktop jump-host orchestrator for self-hosted RustDesk relays.

**Status:** v0.1 in development. Not yet usable — track progress in [`GECK/tasks.md`](GECK/tasks.md).

Rustmote manages a registry of self-hosted RustDesk relay servers, discovers targets on local networks, establishes SSH tunnels to those relays, and launches the RustDesk viewer against the tunneled endpoint. It also manages the full lifecycle of the relay itself — bootstrap, update, rollback — from the client.

## Workspace layout

```
rustmote/
├── crates/
│   ├── rustmote-core/   # library — all real logic lives here
│   └── rustmote-cli/    # binary — clap-based CLI
├── docker/relay/        # docker-compose template for the self-hosted relay
└── docs/                # architecture, security, deployment guides
```

A Tauri GUI crate (`rustmote-gui`) is explicitly deferred to v0.2.

## Quickstart

The quickstart lands in Phase 15 (TASK-015) once the CLI payoff command is wired up. For now, see `RUSTMOTE_SPEC.md` for the authoritative design.

## License

Dual-licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at your option.

---

This repo is managed via the [GECK](https://github.com/) protocol (v1.3). Session handoffs read `GECK/LLM_init.md` (goal), `GECK/tasks.md` (active work), `GECK/decisions.md` + `GECK/learnings.md` (semantic memory), and `GECK/log.md` (recent session narrative).
