//! Subcommand modules.
//!
//! Each module exposes a `Cmd` enum (or `Args` struct) parsed by `clap` and a
//! `run` function consumed from `main`. Phase 1 (TASK-001) ships placeholders;
//! Phases 7–13 fill them in per `RUSTMOTE_SPEC.md` §4.1.
//!
//! Stubs are declared `async` because their final implementations will await
//! on the `rustmote-core` session/registry APIs; suppress the pedantic lint
//! for these stubs so the signatures match the landed impls.
#![allow(clippy::unused_async)]

pub mod config;
pub mod connect;
pub mod relay;
pub mod server;
pub mod status;
pub mod target;
