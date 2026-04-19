//! `rustmote relay` subcommands (Phase 13 / TASK-013).
//!
//! See `RUSTMOTE_SPEC.md` §5.1.

use anyhow::{bail, Result};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum RelayCmd {
    /// One-shot install on a freshly registered server.
    Bootstrap {
        server: String,
        #[arg(long)]
        os: Option<String>,
        #[arg(long)]
        compose_path: Option<String>,
    },
    /// Read-only: check whether newer relay images are available.
    CheckUpdates {
        server: String,
        #[arg(long)]
        json: bool,
    },
    /// Pull newer images, recreate containers, auto-rollback on failure.
    Update {
        server: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        skip_backup: bool,
    },
    /// Show deployed pins, container state, uptime, disk usage.
    Status { server: String },
    /// Tail relay logs over SSH.
    Logs {
        server: String,
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Stop the relay containers.
    Stop { server: String },
    /// Start the relay containers.
    Start { server: String },
    /// Restart the relay containers.
    Restart { server: String },
}

pub async fn run(_cmd: RelayCmd) -> Result<()> {
    bail!("`rustmote relay` subcommands land in Phase 13 (TASK-013).");
}
