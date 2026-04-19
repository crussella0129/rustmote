//! Rustmote CLI entry point.
//!
//! Subcommand handlers live in `commands/`. Phase 1 (TASK-001) ships only the
//! top-level `clap` tree with placeholder subcommands; Phases 7–13 fill them in.

#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::doc_markdown)] // "RustDesk" is a product name, not an identifier

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;

/// Rustmote — Rust-native remote-desktop jump-host orchestrator.
#[derive(Debug, Parser)]
#[command(name = "rustmote", version, about, long_about = None)]
struct Cli {
    /// Increase verbosity (-v = info, -vv = debug, -vvv = trace). Default level is warn.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage registered RustDesk relay servers.
    #[command(subcommand)]
    Server(commands::server::ServerCmd),

    /// Discover and manage target machines.
    #[command(subcommand)]
    Target(commands::target::TargetCmd),

    /// Connect to a target through a registered relay.
    Connect(commands::connect::ConnectArgs),

    /// Show overall Rustmote status.
    Status(commands::status::StatusArgs),

    /// Manage a self-hosted RustDesk relay over SSH.
    #[command(subcommand)]
    Relay(commands::relay::RelayCmd),

    /// Inspect and modify Rustmote configuration.
    #[command(subcommand)]
    Config(commands::config::ConfigCmd),
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Server(cmd) => commands::server::run(cmd).await,
        Command::Target(cmd) => commands::target::run(cmd).await,
        Command::Connect(args) => commands::connect::run(args).await,
        Command::Status(args) => commands::status::run(args).await,
        Command::Relay(cmd) => commands::relay::run(cmd).await,
        Command::Config(cmd) => commands::config::run(cmd).await,
    }
}
