//! Workspace automation.
//!
//! `cargo xtask <command>`, via the alias in `.cargo/config.toml`.

// A binary crate: items are reachable from `main`, not from a public API.
#![allow(unreachable_pub)]

mod proto_sync;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// livekit-rs workspace automation.
#[derive(Debug, Parser)]
#[command(name = "xtask", about = "livekit-rs workspace automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check or update the vendored `livekit/protocol` sources.
    ProtoSync(proto_sync::Args),
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::ProtoSync(args) => proto_sync::run(&args),
    }
}
