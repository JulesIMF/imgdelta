// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress subcommand: reconstruct a target image from a delta

use clap::Args;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image_delta_core::{DecompressOptions, DirectoryImage, Qcow2Image};

use crate::commands::compress::load_config;

#[derive(Args, Debug)]
pub struct DecompressArgs {
    /// Image ID to decompress.
    #[arg(long, value_name = "ID")]
    pub image_id: String,

    /// Path to write the reconstructed image into.
    /// For qcow2 images this is a .qcow2 file path; for directory images a directory path.
    #[arg(long, value_name = "PATH")]
    pub output: PathBuf,

    /// Path to the base image (directory or .qcow2 file, required for delta images).
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

    // Select the image-format driver from the output path extension so that
    // DefaultCompressor::decompress stays format-agnostic: it just calls
    // image_format.create() and then create_partition() for each partition.
    let image_format: Arc<dyn image_delta_core::Image> = {
        let ext = args
            .output
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "qcow2" => Arc::new(Qcow2Image::new()),
            _ => Arc::new(DirectoryImage::new()),
        }
    };

    let base_root: std::path::PathBuf = args.base_image.unwrap_or_default();

    let stats = image_delta_core::operations::decompress(
        image_format,
        Arc::clone(&storage),
        router,
        &args.output,
        DecompressOptions {
            image_id: args.image_id.clone(),
            base_root,
            workers: args.workers.unwrap_or(config.compressor.workers),
        },
    )
    .await?;

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
