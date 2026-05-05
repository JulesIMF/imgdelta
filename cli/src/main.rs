// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// CLI entry point: parse global flags, dispatch to subcommands

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::{fmt, EnvFilter};

mod commands;
mod config;
mod impls;

#[derive(Parser, Debug)]
#[command(
    name = "imgdelta",
    version,
    about = "Delta compression tool for cloud OS images"
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

    /// Override the log level (e.g. debug, info, warn, error).
    #[arg(long, global = true, value_name = "LEVEL")]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Compress a target image relative to a base image.
    Compress(commands::compress::CompressArgs),

    /// Reconstruct an image from stored patches.
    Decompress(commands::decompress::DecompressArgs),

    /// Image management subcommands.
    #[command(subcommand)]
    Image(commands::ImageCommands),

    /// Manifest inspection subcommands.
    #[command(subcommand)]
    Manifest(commands::ManifestCommands),

    /// Debug-only subcommands for inspecting internals against real data.
    ///
    /// Available only in debug builds (`cargo build`).
    #[cfg(debug_assertions)]
    #[command(subcommand)]
    Debug(commands::DebugCommands),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialise tracing subscriber based on --log-level flag or RUST_LOG env var.
    // Default level is "info".  Use --log-level debug for per-file details respectively.
    let level = cli.log_level.as_deref().unwrap_or("info");
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level)),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Commands::Compress(args) => commands::compress::run(args, cli.config.as_deref()).await,
        Commands::Decompress(args) => commands::decompress::run(args, cli.config.as_deref()).await,
        Commands::Image(cmd) => commands::image::run(cmd, cli.config.as_deref()).await,
        Commands::Manifest(cmd) => commands::manifest::run(cmd, cli.config.as_deref()).await,
        #[cfg(debug_assertions)]
        Commands::Debug(cmd) => commands::debug::run(cmd),
    }
}
