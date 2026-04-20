//! `rustmote status` (spec §4.1, Phase 10 / TASK-010).
//!
//! A read-only diagnostic over the local rustmote install. No SSH, no
//! Docker, no network — this is the "what do I have configured and is
//! the viewer present" summary. Per-relay runtime status is a separate
//! command (`rustmote relay status <name>`, Phase 13).
//!
//! Shows:
//!
//! - Resolved config path and credential mode
//! - Default server + count of registered servers/targets
//! - RustDesk viewer detection result (per spec §3.5's per-OS search)
//! - Count of pinned hosts in `known_hosts.toml` (§6.7)
//!
//! `--json` emits the same report as a structured object for scripting
//! per spec §4.2.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use comfy_table::presets::NOTHING;
use comfy_table::{ContentArrangement, Table};
use rustmote_core::config::{config_path, Config};
use rustmote_core::session::KnownHosts;
use rustmote_core::viewer::{Viewer, ViewerKind};
use serde::Serialize;

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: StatusArgs) -> Result<()> {
    let path = config_path().context("resolving config path")?;
    let cfg = Config::load().context("loading config")?;
    let known_hosts = KnownHosts::load().context("loading known_hosts.toml")?;

    let report = StatusReport::build(&cfg, &path, &known_hosts);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serializing status to JSON")?
        );
    } else {
        print_table(&report);
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Report DTO
// -----------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct StatusReport {
    config_path: PathBuf,
    credential_mode: String,
    default_server: Option<String>,
    servers: usize,
    targets: usize,
    viewer: ViewerStatus,
    pinned_hosts: usize,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ViewerStatus {
    Native { path: PathBuf },
    Flatpak,
    NotFound,
}

impl StatusReport {
    fn build(cfg: &Config, config_path: &Path, known_hosts: &KnownHosts) -> Self {
        let viewer = match Viewer::detect_with_override(cfg.general.viewer_override()) {
            Ok(v) => match v.kind() {
                ViewerKind::Native(p) => ViewerStatus::Native { path: p.clone() },
                ViewerKind::Flatpak => ViewerStatus::Flatpak,
            },
            Err(_) => ViewerStatus::NotFound,
        };
        Self {
            config_path: config_path.to_path_buf(),
            credential_mode: cfg.general.credential_mode.to_string(),
            default_server: cfg.general.default_server.clone(),
            servers: cfg.servers().len(),
            targets: cfg.targets().len(),
            viewer,
            pinned_hosts: known_hosts.entries.len(),
        }
    }
}

fn print_table(r: &StatusReport) {
    let mut table = Table::new();
    table
        .load_preset(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["KEY", "VALUE"]);
    table.add_row(vec![
        "config_path".to_string(),
        r.config_path.display().to_string(),
    ]);
    table.add_row(vec![
        "credential_mode".to_string(),
        r.credential_mode.clone(),
    ]);
    table.add_row(vec![
        "default_server".to_string(),
        r.default_server
            .clone()
            .unwrap_or_else(|| "(none)".to_string()),
    ]);
    table.add_row(vec!["servers".to_string(), r.servers.to_string()]);
    table.add_row(vec!["targets".to_string(), r.targets.to_string()]);
    table.add_row(vec![
        "viewer".to_string(),
        match &r.viewer {
            ViewerStatus::Native { path } => format!("native: {}", path.display()),
            ViewerStatus::Flatpak => "flatpak (com.rustdesk.RustDesk)".to_string(),
            ViewerStatus::NotFound => "not found".to_string(),
        },
    ]);
    table.add_row(vec!["pinned_hosts".to_string(), r.pinned_hosts.to_string()]);
    println!("{table}");
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn report_build_on_empty_config_has_sensible_defaults() {
        let cfg = Config::default();
        let known = KnownHosts::default();
        let report = StatusReport::build(&cfg, &PathBuf::from("/tmp/config.toml"), &known);
        assert_eq!(report.credential_mode, "prompt");
        assert_eq!(report.servers, 0);
        assert_eq!(report.targets, 0);
        assert_eq!(report.pinned_hosts, 0);
        assert!(report.default_server.is_none());
    }

    #[test]
    fn json_emits_structured_shape() {
        let cfg = Config::default();
        let known = KnownHosts::default();
        let report = StatusReport::build(&cfg, &PathBuf::from("/tmp/c.toml"), &known);
        let json = serde_json::to_string_pretty(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["credential_mode"], "prompt");
        assert_eq!(parsed["servers"], 0);
        assert!(parsed["viewer"].is_object());
        // Viewer status tag should be one of the known variants.
        let tag = parsed["viewer"]["status"].as_str().unwrap();
        assert!(matches!(tag, "native" | "flatpak" | "not_found"));
    }
}
