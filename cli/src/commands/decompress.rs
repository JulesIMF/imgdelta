// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress subcommand: reconstruct a target image from a delta

use clap::Args;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image_delta_core::{Compressor, DecompressOptions, DefaultCompressor, DirectoryImage};

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
    /// Omit only if the image was compressed without a base (first image).
    #[arg(long, value_name = "PATH")]
    pub base_image: Option<PathBuf>,

    /// Number of parallel worker threads (overrides config).
    #[arg(long, value_name = "N")]
    pub workers: Option<usize>,
}

pub async fn run(args: DecompressArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;

    let storage = config.storage.build().await?;
    let router = config.compressor.build_router()?;
    let compressor = DefaultCompressor::new(
        Arc::new(DirectoryImage::new()),
        Arc::clone(&storage),
        router,
    );

    // When no base is provided, use an empty temp directory so that all files
    // are reconstructed from stored blobs (first-image bootstrap case).
    let _empty_tmp;
    let base_root: PathBuf = match args.base_image {
        Some(p) => p,
        None => {
            _empty_tmp =
                tempfile::tempdir().map_err(|e| anyhow::anyhow!("cannot create temp dir: {e}"))?;
            _empty_tmp.path().to_path_buf()
        }
    };

    let opts = DecompressOptions {
        image_id: args.image_id.clone(),
        base_root,
        workers: args.workers.unwrap_or(config.compressor.workers),
    };

    let stats = compressor.decompress(&args.output, opts).await?;

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
