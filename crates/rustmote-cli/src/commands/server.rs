//! `rustmote server` subcommands (Phase 7 / TASK-007).
//!
//! See `RUSTMOTE_SPEC.md` §4.1. Phase 1 stubs parse the subcommand tree and
//! exit with `unimplemented`; real handlers land in TASK-007.

use anyhow::{bail, Result};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ServerCmd {
    /// Add a server to the registry.
    Add {
        name: String,
        #[arg(long)]
        host: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long, default_value_t = 22)]
        ssh_port: u16,
        #[arg(long, default_value_t = 21116)]
        relay_port: u16,
    },
    /// List registered servers.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a server from the registry.
    Remove { name: String },
    /// Show details for a single server.
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(_cmd: ServerCmd) -> Result<()> {
    bail!("`rustmote server` subcommands land in Phase 7 (TASK-007).");
}
