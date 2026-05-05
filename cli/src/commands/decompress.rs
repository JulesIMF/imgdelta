// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress subcommand: reconstruct a target image from a delta

use clap::Args;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image_delta_core::image::{Image, PartitionHandle};
use image_delta_core::{
    Compressor, DecompressOptions, DefaultCompressor, DirectoryImage, Qcow2Image,
};

use crate::commands::compress::load_config;

#[derive(Args, Debug)]
pub struct DecompressArgs {
    /// Image ID to decompress.
    #[arg(long, value_name = "ID")]
    pub image_id: String,

    /// Directory to write the reconstructed filesystem into.
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
    let compressor = DefaultCompressor::new(
        Arc::new(DirectoryImage::new()),
        Arc::clone(&storage),
        router,
    );

    // When no base is provided, use an empty temp directory so that all files
    // are reconstructed from stored blobs (first-image bootstrap case).
    let _empty_tmp;
    // Keep qcow2 mount handles alive for the duration of decompress.
    let _qcow2_mounts: Vec<Box<dyn image_delta_core::image::MountHandle>>;
    let _qcow2_open: Option<Box<dyn image_delta_core::image::OpenImage>>;

    let base_root: PathBuf = match args.base_image {
        None => {
            _empty_tmp =
                tempfile::tempdir().map_err(|e| anyhow::anyhow!("cannot create temp dir: {e}"))?;
            _qcow2_mounts = Vec::new();
            _qcow2_open = None;
            _empty_tmp.path().to_path_buf()
        }
        Some(p) if p.extension().and_then(|e| e.to_str()) == Some("qcow2") => {
            // Mount the base qcow2 and use its first Fs partition as base_root.
            let img = Qcow2Image::new();
            let open = img.open(&p)?;
            let parts = open.partitions()?;
            let mut mounts: Vec<Box<dyn image_delta_core::image::MountHandle>> = Vec::new();
            let mut fs_root: Option<PathBuf> = None;
            for ph in parts {
                if let PartitionHandle::Fs(handle) = ph {
                    let m = handle.mount()?;
                    if fs_root.is_none() {
                        fs_root = Some(m.root().to_path_buf());
                    }
                    mounts.push(m);
                }
            }
            let root = fs_root
                .ok_or_else(|| anyhow::anyhow!("base qcow2 has no Fs partition to use as base"))?;
            _empty_tmp =
                tempfile::tempdir().map_err(|e| anyhow::anyhow!("cannot create temp dir: {e}"))?;
            _qcow2_mounts = mounts;
            _qcow2_open = Some(open);
            root
        }
        Some(p) => {
            _empty_tmp =
                tempfile::tempdir().map_err(|e| anyhow::anyhow!("cannot create temp dir: {e}"))?;
            _qcow2_mounts = Vec::new();
            _qcow2_open = None;
            p
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
