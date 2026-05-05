// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// compress subcommand: compress a target image against a base

use clap::Args;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image_delta_core::{Compressor, DefaultCompressor, DirectoryImage};

use crate::config::{CompressorConfig, Config, EncoderKind, StorageConfig};

#[derive(Args, Debug)]
pub struct CompressArgs {
    /// Path to the target image to compress.
    #[arg(long, value_name = "PATH")]
    pub image: PathBuf,

    /// Path to the base image used as delta source.
    /// Omit for the very first (base) image — an empty directory is assumed.
    #[arg(long, value_name = "PATH")]
    pub base_image: Option<PathBuf>,

    /// Provider-assigned identifier for the target image.
    #[arg(long, value_name = "ID")]
    pub image_id: String,

    /// Provider-assigned identifier for the base image.
    /// Omit when compressing the first image with no prior base.
    #[arg(long, value_name = "ID")]
    pub base_image_id: Option<String>,

    /// Image format override. Detected from file extension if omitted.
    #[arg(long, value_name = "FORMAT", value_parser = ["directory", "qcow2"])]
    pub image_format: Option<String>,

    /// Number of parallel worker threads (overrides config).
    #[arg(long, value_name = "N")]
    pub workers: Option<usize>,

    /// Overwrite an existing image with the same image-id instead of returning
    /// an error.
    #[arg(long, default_value_t = false)]
    pub overwrite: bool,
}

pub async fn run(args: CompressArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;

    let storage = config.storage.build().await?;
    let router = config.compressor.build_router()?;
    let compressor = DefaultCompressor::new(
        Arc::new(DirectoryImage::new()),
        Arc::clone(&storage),
        router,
    );

    // When no base is provided, use an empty temp directory as source so the
    // compressor stores all files as "added" blobs (first-image bootstrap).
    let _empty_tmp;
    let base_root: &Path = match &args.base_image {
        Some(p) => p.as_path(),
        None => {
            _empty_tmp =
                tempfile::tempdir().map_err(|e| anyhow::anyhow!("cannot create temp dir: {e}"))?;
            _empty_tmp.path()
        }
    };

    let opts = image_delta_core::CompressOptions {
        image_id: args.image_id.clone(),
        base_image_id: args.base_image_id.clone(),
        workers: args.workers.unwrap_or(config.compressor.workers),
        passthrough_threshold: config.compressor.passthrough_threshold,
        overwrite: args.overwrite,
    };

    let stats = compressor.compress(base_root, &args.image, opts).await?;

    let base_label = args.base_image_id.as_deref().unwrap_or("(none)");
    eprintln!(
        "Compressed {} → {}\n  base={}, added={}, patched={}, removed={}, source_bytes={}, stored_bytes={}, elapsed={:.2}s",
        base_label,
        args.image_id,
        base_label,
        stats.files_added,
        stats.files_patched,
        stats.files_removed,
        stats.total_source_bytes,
        stats.total_stored_bytes,
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
