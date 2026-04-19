//! Rustmote core library.
//!
//! Provides the registry, credential handling, SSH session orchestration,
//! RustDesk viewer invocation, LAN discovery, and self-hosted relay
//! lifecycle management primitives used by the `rustmote-cli` binary.
//!
//! See `RUSTMOTE_SPEC.md` §3 for the authoritative module contract.
//!
//! # Pedantic lint policy
//!
//! CI runs `cargo clippy --all-targets -- -D warnings -W clippy::pedantic`
//! per spec §6.6. Lint-level attributes here allow-list specific pedantic
//! lints that are mismatched with the spec-dictated API shape.

#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)] // matches spec-dictated naming (e.g. `RustmoteError`)
#![allow(clippy::missing_errors_doc)] // variants are self-documenting via `thiserror`
#![allow(clippy::missing_panics_doc)] // zero-panic target; where a panic exists, it is invariant-guarded
#![allow(clippy::doc_markdown)] // "RustDesk" is a product name, not an identifier

pub mod config;
pub mod credentials;
pub mod discovery;
pub mod error;
pub mod registry;
pub mod registry_client;
pub mod relay_lifecycle;
pub mod session;
pub mod target;
pub mod viewer;

pub use error::RustmoteError;

/// Result type used throughout `rustmote-core`.
pub type Result<T> = std::result::Result<T, RustmoteError>;
