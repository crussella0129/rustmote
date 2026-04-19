//! `rustmote target` subcommands (Phase 8 / TASK-008).
//!
//! See `RUSTMOTE_SPEC.md` §4.1.

use anyhow::{bail, Result};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum TargetCmd {
    /// Scan the local network for RustDesk-capable hosts.
    Scan {
        #[arg(long)]
        cidr: Option<String>,
        #[arg(long, default_value_t = 10)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// List known targets.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a target to the registry.
    Add {
        id: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        via: Option<String>,
    },
    /// Remove a target from the registry.
    Remove { id: String },
}

pub async fn run(_cmd: TargetCmd) -> Result<()> {
    bail!("`rustmote target` subcommands land in Phase 8 (TASK-008).");
}
