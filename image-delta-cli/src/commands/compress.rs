use clap::Args;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image_delta_core::{Compressor, DefaultCompressor};

use crate::config::{CompressorConfig, Config, EncoderKind, StorageConfig};

#[derive(Args, Debug)]
pub struct CompressArgs {
    /// Path to the target image to compress.
    #[arg(long, value_name = "PATH")]
    pub image: PathBuf,

    /// Path to the base image used as delta source.
    #[arg(long, value_name = "PATH")]
    pub base_image: PathBuf,

    /// Provider-assigned identifier for the target image.
    #[arg(long, value_name = "ID")]
    pub image_id: String,

    /// Provider-assigned identifier for the base image.
    #[arg(long, value_name = "ID")]
    pub base_image_id: String,

    /// Image format override. Detected from file extension if omitted.
    #[arg(long, value_name = "FORMAT", value_parser = ["directory", "qcow2"])]
    pub image_format: Option<String>,

    /// Number of parallel worker threads (overrides config).
    #[arg(long, value_name = "N")]
    pub workers: Option<usize>,
}

pub async fn run(args: CompressArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;

    let storage = config.storage.build().await?;
    let router = config.compressor.build_router()?;
    let compressor = DefaultCompressor::new(Arc::clone(&storage), router);

    let opts = image_delta_core::CompressOptions {
        image_id: args.image_id.clone(),
        base_image_id: Some(args.base_image_id.clone()),
        workers: args.workers.unwrap_or(config.compressor.workers),
        passthrough_threshold: config.compressor.passthrough_threshold,
    };

    let stats = compressor
        .compress(&args.base_image, &args.image, opts)
        .await?;

    eprintln!(
        "Compressed {} → {}\n  added={}, patched={}, removed={}, elapsed={:.2}s",
        args.base_image_id,
        args.image_id,
        stats.files_added,
        stats.files_patched,
        stats.files_removed,
        stats.elapsed_secs,
    );
    Ok(())
}

/// Load config from `path` (if given) or fall back to a default local-storage config.
pub fn load_config(path: Option<&Path>) -> anyhow::Result<Config> {
    if let Some(p) = path {
        return Config::from_file(p);
    }
    // Default: local storage in the current directory under .imgdelta/
    let local_dir = std::env::current_dir()?.join(".imgdelta");
    Ok(Config {
        storage: StorageConfig::Local { local_dir },
        compressor: CompressorConfig {
            workers: 4,
            passthrough_threshold: 1.0,
            default_encoder: EncoderKind::Xdelta3,
            routing: Vec::new(),
        },
        logging: Default::default(),
    })
}
