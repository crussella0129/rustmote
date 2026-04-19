//! `rustmote status` (Phase 10 / TASK-010).
//!
//! See `RUSTMOTE_SPEC.md` §4.1.

use anyhow::{bail, Result};
use clap::Args;

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[arg(long)]
    pub json: bool,
}

pub async fn run(_args: StatusArgs) -> Result<()> {
    bail!("`rustmote status` lands in Phase 10 (TASK-010).");
}
