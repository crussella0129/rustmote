//! `rustmote config` (spec §4.1, Phase 10 / TASK-010).
//!
//! Three subcommands:
//!
//! - `config path` — print the resolved `config.toml` path for this OS.
//!   Landed early in TASK-007 so the spec §7.3 smoke test had a handler
//!   to exercise.
//! - `config show [--json]` — render the current config state. Default
//!   output is a minimal `comfy-table` key/value view per spec §4.2.
//!   `--json` emits `Config` serialized via `serde_json` for scripting.
//! - `config set-mode <mode> [--i-understand-this-is-insecure]` — flip
//!   `general.credential_mode`. Setting `unsafe` without the ack flag
//!   is a hard refusal per spec §6.2; the CLI layer is the only gate —
//!   `rustmote-core::credentials` already refuses a `credentials.toml`
//!   whose permissions exceed `0600`, but it can't know whether the
//!   user ever consented to the mode in the first place.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use comfy_table::presets::NOTHING;
use comfy_table::{ContentArrangement, Table};
use rustmote_core::config::{config_path, Config};
use rustmote_core::credentials::CredentialMode;

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
    match cmd {
        ConfigCmd::Path => {
            let p = config_path().context("resolving config path")?;
            println!("{}", p.display());
            Ok(())
        }
        ConfigCmd::Show { json } => {
            let path = config_path().context("resolving config path")?;
            let cfg = Config::load().context("loading config")?;
            print_show(&cfg, &path, json)
        }
        ConfigCmd::SetMode { mode, ack_unsafe } => {
            let parsed = parse_and_gate_mode(&mode, ack_unsafe)?;
            let mut cfg = Config::load().context("loading config")?;
            if apply_mode(&mut cfg, parsed) {
                cfg.save().context("saving config")?;
            }
            Ok(())
        }
    }
}

// -----------------------------------------------------------------------------
// Pure helpers — small enough to unit test without touching the filesystem.
// -----------------------------------------------------------------------------

fn print_show(cfg: &Config, path: &Path, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(cfg).context("serializing config to JSON")?
        );
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["KEY", "VALUE"]);
    table.add_row(vec!["config_path".to_string(), path.display().to_string()]);
    table.add_row(vec![
        "credential_mode".to_string(),
        cfg.general.credential_mode.to_string(),
    ]);
    table.add_row(vec![
        "default_server".to_string(),
        cfg.general
            .default_server
            .clone()
            .unwrap_or_else(|| "(none)".to_string()),
    ]);
    table.add_row(vec![
        "viewer_path".to_string(),
        if cfg.general.viewer_path.is_empty() {
            "(auto-detect)".to_string()
        } else {
            cfg.general.viewer_path.clone()
        },
    ]);
    table.add_row(vec!["servers".to_string(), cfg.servers().len().to_string()]);
    table.add_row(vec!["targets".to_string(), cfg.targets().len().to_string()]);
    println!("{table}");
    Ok(())
}

/// Parse `mode` and enforce the spec §6.2 `unsafe` acknowledgement gate.
/// Separated from the load/save path so unit tests can exercise the gate
/// without touching disk.
fn parse_and_gate_mode(mode: &str, ack_unsafe: bool) -> Result<CredentialMode> {
    let parsed: CredentialMode = mode.parse().with_context(|| {
        format!("'{mode}' is not a valid credential mode (expected: prompt | keychain | unsafe)")
    })?;
    if parsed == CredentialMode::Unsafe && !ack_unsafe {
        bail!(
            "refusing to enable unsafe mode without --i-understand-this-is-insecure (spec §6.2). \
             Unsafe mode stores plaintext SSH passwords in $CONFIG/rustmote/credentials.toml \
             with mode 0600 and logs a warning on every access. Prefer 'keychain' where available."
        );
    }
    Ok(parsed)
}

/// Mutate `cfg.general.credential_mode` to `new` and print a one-line
/// transition summary to stdout. Returns `true` when a real transition
/// happened (caller should persist), `false` when it was a no-op.
fn apply_mode(cfg: &mut Config, new: CredentialMode) -> bool {
    let previous = cfg.general.credential_mode;
    if previous == new {
        println!("credential_mode already {new}; no change");
        return false;
    }
    cfg.general.credential_mode = new;
    println!("credential_mode: {previous} -> {new}");
    if new == CredentialMode::Unsafe {
        eprintln!(
            "warning: plaintext credentials will be written to \
             $CONFIG/rustmote/credentials.toml (mode 0600 on Unix)"
        );
    }
    true
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_refuses_unsafe_without_ack() {
        let err = parse_and_gate_mode("unsafe", false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("--i-understand-this-is-insecure"));
        assert!(msg.contains("§6.2"));
    }

    #[test]
    fn gate_accepts_unsafe_with_ack() {
        let parsed = parse_and_gate_mode("unsafe", true).unwrap();
        assert_eq!(parsed, CredentialMode::Unsafe);
    }

    #[test]
    fn gate_accepts_keychain_without_ack() {
        // Ack flag is irrelevant for non-unsafe modes — it only gates
        // the one mode that needs a conscience check.
        let parsed = parse_and_gate_mode("keychain", false).unwrap();
        assert_eq!(parsed, CredentialMode::Keychain);
    }

    #[test]
    fn gate_rejects_unknown_mode() {
        let err = parse_and_gate_mode("bogus", false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("bogus"));
        assert!(msg.contains("prompt"));
        assert!(msg.contains("keychain"));
    }

    #[test]
    fn apply_mode_changes_credential_mode_and_signals_save() {
        let mut cfg = Config::default();
        assert_eq!(cfg.general.credential_mode, CredentialMode::Prompt);
        let changed = apply_mode(&mut cfg, CredentialMode::Keychain);
        assert!(changed);
        assert_eq!(cfg.general.credential_mode, CredentialMode::Keychain);
    }

    #[test]
    fn apply_mode_noop_returns_false() {
        let mut cfg = Config::default();
        let changed = apply_mode(&mut cfg, CredentialMode::Prompt);
        assert!(!changed);
        assert_eq!(cfg.general.credential_mode, CredentialMode::Prompt);
    }

    #[test]
    fn show_json_serializes_config_with_defaults() {
        let cfg = Config::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["general"]["credential_mode"], "prompt");
    }
}
