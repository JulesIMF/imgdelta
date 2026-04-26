use clap::Args;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image_delta_core::{Compressor, DecompressOptions, DefaultCompressor};

use crate::commands::compress::load_config;

#[derive(Args, Debug)]
pub struct DecompressArgs {
    /// Image ID to decompress.
    #[arg(long, value_name = "ID")]
    pub image_id: String,

    /// Directory to write the reconstructed filesystem into.
    #[arg(long, value_name = "PATH")]
    pub output: PathBuf,

    /// Path to the base image directory (required for delta images).
    #[arg(long, value_name = "PATH")]
    pub base_image: PathBuf,

    /// Number of parallel worker threads (overrides config).
    #[arg(long, value_name = "N")]
    pub workers: Option<usize>,
}

pub fn run(args: DecompressArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;

    let storage = config.storage.build()?;
    let encoder = config.compressor.build_encoder()?;
    let compressor = DefaultCompressor::new(Arc::clone(&storage), encoder);

    let opts = DecompressOptions {
        image_id: args.image_id.clone(),
        base_root: args.base_image.clone(),
        workers: args.workers.unwrap_or(config.compressor.workers),
    };

    let stats = compressor.decompress(&args.output, opts)?;

    eprintln!(
        "Decompressed {} → {}\n  files={}, bytes={}, elapsed={:.2}s",
        args.image_id,
        args.output.display(),
        stats.total_files,
        stats.total_bytes,
        stats.elapsed_secs,
    );
    Ok(())
}
