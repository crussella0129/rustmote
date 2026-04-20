//! `rustmote target` subcommands (spec §4.1, Phase 8 / TASK-008).
//!
//! Scan the LAN for candidate hosts, list registered targets, and CRUD
//! `Target` entries in the registry. Scans use `rustmote_core::discovery`
//! under an `indicatif` spinner; the spec §3.6 scan is concurrent mDNS +
//! ICMP + ARP with a 10 s /24 budget, so progress is reported as
//! indeterminate "working…" rather than a percentage bar.
//!
//! Target IDs are validated through `rustmote_core::viewer::TargetId`
//! (spec §3.5 regex `^[0-9]{9,10}$`) at add-time — the same validated
//! newtype used in the viewer invocation path. `--via` must resolve to
//! a server already in the registry so we fail fast rather than deferring
//! the error to `connect` (spec §13 open question resolved in favor of
//! eager validation).
//!
//! `--json` on list / show / scan follows spec §4.2: data to stdout,
//! tracing logs to stderr.

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Subcommand;
use comfy_table::presets::NOTHING;
use comfy_table::{Cell, ContentArrangement, Table};
use indicatif::{ProgressBar, ProgressStyle};
use ipnet::Ipv4Net;
use rustmote_core::config::Config;
use rustmote_core::discovery::{DiscoveredHost, Discovery, DEFAULT_SCAN_TIMEOUT};
use rustmote_core::target::Target;
use rustmote_core::viewer::TargetId;
use serde::Serialize;

#[derive(Debug, Subcommand)]
pub enum TargetCmd {
    /// Scan the local network for candidate hosts.
    Scan {
        /// CIDR to sweep (e.g. `192.168.1.0/24`). Auto-detected when omitted.
        #[arg(long)]
        cidr: Option<String>,
        /// Overall scan timeout in seconds. Spec §3.6 budgets 10 s for /24.
        #[arg(long)]
        timeout: Option<u64>,
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
        /// RustDesk ID — 9 or 10 ASCII digits (spec §3.5).
        id: String,
        /// Human-friendly label.
        #[arg(long)]
        label: Option<String>,
        /// Name of the relay server used to reach this target; must be
        /// registered (use `rustmote server add` first).
        #[arg(long)]
        via: Option<String>,
    },
    /// Remove a target from the registry.
    Remove { id: String },
}

pub async fn run(cmd: TargetCmd) -> Result<()> {
    match cmd {
        TargetCmd::Scan {
            cidr,
            timeout,
            json,
        } => run_scan(cidr, timeout, json).await,
        TargetCmd::List { json } => run_list(json),
        TargetCmd::Add { id, label, via } => run_add(&id, label, via),
        TargetCmd::Remove { id } => run_remove(&id),
    }
}

// -----------------------------------------------------------------------------
// scan
// -----------------------------------------------------------------------------

#[derive(Serialize)]
struct ScanEntry {
    ip: IpAddr,
    hostname: Option<String>,
    mac: Option<String>,
    is_known_server: bool,
}

impl From<&DiscoveredHost> for ScanEntry {
    fn from(h: &DiscoveredHost) -> Self {
        Self {
            ip: h.ip,
            hostname: h.hostname.clone(),
            mac: h.mac.clone(),
            is_known_server: h.is_known_server,
        }
    }
}

async fn run_scan(cidr: Option<String>, timeout: Option<u64>, json: bool) -> Result<()> {
    let cfg = Config::load().context("loading config")?;
    let known: Vec<IpAddr> = cfg.servers().iter().map(|s| s.host).collect();

    let mut driver = Discovery::new().with_known_servers(known);
    if let Some(c) = cidr {
        let parsed: Ipv4Net = c.parse().with_context(|| format!("parsing --cidr '{c}'"))?;
        driver = driver.with_cidr(parsed);
    }
    let budget = timeout.map_or(DEFAULT_SCAN_TIMEOUT, Duration::from_secs);
    driver = driver.with_timeout(budget);

    let spinner = if json {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.set_message(format!("scanning (budget {}s)", budget.as_secs()));
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    };

    let hosts = driver.scan().await.context("running LAN discovery sweep")?;
    spinner.finish_and_clear();

    if json {
        let entries: Vec<ScanEntry> = hosts.iter().map(ScanEntry::from).collect();
        let out = serde_json::to_string_pretty(&entries).context("serializing scan as JSON")?;
        println!("{out}");
        return Ok(());
    }

    if hosts.is_empty() {
        println!("no hosts found on scanned subnet");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("IP"),
            Cell::new("HOSTNAME"),
            Cell::new("MAC"),
            Cell::new("KNOWN"),
        ]);
    for h in &hosts {
        table.add_row(vec![
            Cell::new(h.ip),
            Cell::new(h.hostname.clone().unwrap_or_else(|| "—".to_owned())),
            Cell::new(h.mac.clone().unwrap_or_else(|| "—".to_owned())),
            Cell::new(if h.is_known_server { "server" } else { "—" }),
        ]);
    }
    println!("{table}");
    Ok(())
}

// -----------------------------------------------------------------------------
// list
// -----------------------------------------------------------------------------

fn run_list(json: bool) -> Result<()> {
    let cfg = Config::load().context("loading config")?;
    let targets = cfg.targets();

    if json {
        let out = serde_json::to_string_pretty(targets).context("serializing targets as JSON")?;
        println!("{out}");
        return Ok(());
    }

    if targets.is_empty() {
        println!("no targets registered (use `rustmote target add <id>`)");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("ID"),
            Cell::new("LABEL"),
            Cell::new("VIA"),
            Cell::new("LAST IP"),
            Cell::new("LAST SEEN"),
        ]);
    for t in targets {
        table.add_row(vec![
            Cell::new(&t.id),
            Cell::new(t.label.clone().unwrap_or_else(|| "—".to_owned())),
            Cell::new(t.via_server.clone().unwrap_or_else(|| "—".to_owned())),
            Cell::new(t.ip.map_or_else(|| "—".to_owned(), |ip| ip.to_string())),
            Cell::new(
                t.last_seen
                    .map_or_else(|| "—".to_owned(), |d| d.format("%Y-%m-%d").to_string()),
            ),
        ]);
    }
    println!("{table}");
    Ok(())
}

// -----------------------------------------------------------------------------
// add / remove
// -----------------------------------------------------------------------------

fn run_add(id: &str, label: Option<String>, via: Option<String>) -> Result<()> {
    // Validate via TargetId — same newtype the viewer invocation path uses,
    // so the spec §3.5 regex is enforced at exactly one gate.
    TargetId::new(id).context("validating target id against spec §3.5")?;

    let mut cfg = Config::load().context("loading config")?;

    if let Some(ref server) = via {
        cfg.get_server(server)
            .with_context(|| format!("--via '{server}' is not in the registry"))?;
    }

    let target = Target {
        id: id.to_owned(),
        ip: None,
        label,
        via_server: via,
        last_seen: None,
    };
    cfg.add_target(target)
        .context("adding target to registry")?;
    cfg.save().context("saving config")?;

    println!("added target '{id}'");
    Ok(())
}

fn run_remove(id: &str) -> Result<()> {
    let mut cfg = Config::load().context("loading config")?;
    let removed = cfg.remove_target(id).context("removing target")?;
    cfg.save().context("saving config")?;
    println!("removed target '{}'", removed.id);
    Ok(())
}
