use clap::{Parser, Subcommand};
use std::path::PathBuf;

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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // TODO Phase 6: initialise tracing + indicatif based on cli.log_level and config

    match cli.command {
        Commands::Compress(args) => commands::compress::run(args),
        Commands::Decompress(args) => commands::decompress::run(args),
        Commands::Image(cmd) => commands::image::run(cmd),
        Commands::Manifest(cmd) => commands::manifest::run(cmd),
        #[cfg(debug_assertions)]
        Commands::Debug(cmd) => commands::debug::run(cmd),
    }
}
