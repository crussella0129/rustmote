//! `rustmote server` subcommands (spec §4.1, Phase 7 / TASK-007).
//!
//! Add, list, remove, and inspect `RemoteServer` entries in the registry.
//! Table output uses `comfy-table` with a minimal borderless style; `--json`
//! on `list` / `show` emits structured JSON for scripting (data → stdout,
//! tracing logs → stderr, per spec §4.2).
//!
//! Prompting follows spec §4.2: `dialoguer` is used only when a required
//! flag is missing AND stdin is a TTY. Non-TTY invocations without the flag
//! bail with an explicit message naming the flag.

use std::io::IsTerminal;
use std::net::IpAddr;

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use comfy_table::presets::NOTHING;
use comfy_table::{Cell, ContentArrangement, Table};
use rustmote_core::config::Config;
use rustmote_core::registry::RemoteServer;

#[derive(Debug, Subcommand)]
pub enum ServerCmd {
    /// Add a server to the registry.
    Add {
        /// Server name (spec §6.4: `^[a-zA-Z0-9_-]{1,64}$`).
        name: String,
        #[arg(long)]
        host: IpAddr,
        /// SSH username; prompted if omitted on a TTY, else required.
        #[arg(long)]
        user: Option<String>,
        #[arg(long, default_value_t = RemoteServer::DEFAULT_SSH_PORT)]
        ssh_port: u16,
        #[arg(long, default_value_t = RemoteServer::DEFAULT_RELAY_PORT)]
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

pub async fn run(cmd: ServerCmd) -> Result<()> {
    match cmd {
        ServerCmd::Add {
            name,
            host,
            user,
            ssh_port,
            relay_port,
        } => run_add(&name, host, user, ssh_port, relay_port),
        ServerCmd::List { json } => run_list(json),
        ServerCmd::Remove { name } => run_remove(&name),
        ServerCmd::Show { name, json } => run_show(&name, json),
    }
}

fn run_add(
    name: &str,
    host: IpAddr,
    user: Option<String>,
    ssh_port: u16,
    relay_port: u16,
) -> Result<()> {
    RemoteServer::validate_name(name).context("validating server name against spec §6.4")?;

    let user = resolve_user(user, name)?;

    let mut cfg = Config::load().context("loading config")?;
    cfg.add_server(RemoteServer::new(name, host, user, ssh_port, relay_port))
        .context("adding server to registry")?;
    cfg.save().context("saving config")?;

    println!("added server '{name}' ({host})");
    Ok(())
}

fn resolve_user(user: Option<String>, server_name: &str) -> Result<String> {
    if let Some(u) = user {
        return Ok(u);
    }
    if std::io::stdin().is_terminal() {
        let prompt = format!("SSH username for '{server_name}'");
        let input: String = dialoguer::Input::new()
            .with_prompt(prompt)
            .interact_text()
            .context("reading SSH username from prompt")?;
        return Ok(input);
    }
    Err(anyhow!(
        "--user is required when stdin is not a TTY (pass --user <name>)"
    ))
}

fn run_list(json: bool) -> Result<()> {
    let cfg = Config::load().context("loading config")?;
    let servers = cfg.servers();

    if json {
        let out = serde_json::to_string_pretty(servers).context("serializing servers as JSON")?;
        println!("{out}");
        return Ok(());
    }

    if servers.is_empty() {
        println!("no servers registered (use `rustmote server add <name> --host <ip>`)");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("NAME"),
            Cell::new("HOST"),
            Cell::new("SSH"),
            Cell::new("USER"),
            Cell::new("RELAY"),
            Cell::new("ADDED"),
        ]);
    for s in servers {
        table.add_row(vec![
            Cell::new(&s.name),
            Cell::new(s.host),
            Cell::new(s.ssh_port),
            Cell::new(&s.ssh_user),
            Cell::new(s.relay_port),
            Cell::new(s.created_at.format("%Y-%m-%d")),
        ]);
    }
    println!("{table}");
    Ok(())
}

fn run_remove(name: &str) -> Result<()> {
    let mut cfg = Config::load().context("loading config")?;
    let removed = cfg.remove_server(name).context("removing server")?;
    cfg.save().context("saving config")?;
    println!("removed server '{}' ({})", removed.name, removed.host);
    Ok(())
}

fn run_show(name: &str, json: bool) -> Result<()> {
    let cfg = Config::load().context("loading config")?;
    let server = cfg.get_server(name).context("looking up server")?;

    if json {
        let out = serde_json::to_string_pretty(server).context("serializing server as JSON")?;
        println!("{out}");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic);
    let rows: Vec<(&str, String)> = vec![
        ("name", server.name.clone()),
        ("host", server.host.to_string()),
        ("ssh_port", server.ssh_port.to_string()),
        ("ssh_user", server.ssh_user.clone()),
        ("relay_port", server.relay_port.to_string()),
        (
            "relay_key",
            server.relay_key.clone().unwrap_or_else(|| "—".to_owned()),
        ),
        ("created_at", server.created_at.to_rfc3339()),
        (
            "last_used",
            server
                .last_used
                .map_or_else(|| "—".to_owned(), |d| d.to_rfc3339()),
        ),
    ];
    for (k, v) in rows {
        table.add_row(vec![Cell::new(k), Cell::new(v)]);
    }
    println!("{table}");
    Ok(())
}
