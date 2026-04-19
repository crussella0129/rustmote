//! `rustmote connect` (Phase 9 / TASK-009).
//!
//! See `RUSTMOTE_SPEC.md` §4.1.

use anyhow::{bail, Result};
use clap::Args;

#[derive(Debug, Args)]
pub struct ConnectArgs {
    /// Target ID or label.
    pub target: String,
    /// Override the configured default server.
    #[arg(long)]
    pub via: Option<String>,
    /// Override SSH user for the underlying tunnel.
    #[arg(long)]
    pub user: Option<String>,
}

pub async fn run(_args: ConnectArgs) -> Result<()> {
    bail!("`rustmote connect` lands in Phase 9 (TASK-009).");
}
