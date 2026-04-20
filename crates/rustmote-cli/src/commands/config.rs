//! `rustmote config` (Phase 10 / TASK-010).
//!
//! See `RUSTMOTE_SPEC.md` §4.1. The `set-mode unsafe` path is gated on
//! `--i-understand-this-is-insecure` per §6.2.

use anyhow::{bail, Result};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Show the current configuration.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Set the credential mode (prompt | keychain | unsafe).
    SetMode {
        mode: String,
        #[arg(long = "i-understand-this-is-insecure")]
        ack_unsafe: bool,
    },
    /// Print the resolved config file path.
    Path,
}

pub async fn run(cmd: ConfigCmd) -> Result<()> {
    // `Path` is a trivial read of the resolved config directory — landed
    // early so the spec §7.3 CLI smoke test for `rustmote config path` can
    // pass. `Show` and `SetMode` land in Phase 10 (TASK-010).
    match cmd {
        ConfigCmd::Path => {
            let p = rustmote_core::config::config_path()?;
            println!("{}", p.display());
            Ok(())
        }
        ConfigCmd::Show { .. } | ConfigCmd::SetMode { .. } => {
            bail!("`rustmote config show` / `set-mode` land in Phase 10 (TASK-010).")
        }
    }
}
