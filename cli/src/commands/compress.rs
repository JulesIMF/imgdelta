// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// compress subcommand: compress a target image against a base

use clap::Args;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image_delta_core::{Compressor, DefaultCompressor, DirectoryImage, Qcow2Image};

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

    /// If set, each pipeline stage dumps a JSON snapshot of the draft into
    /// this directory as `<NN>_<stage>.json` (for debugging rename matching).
    #[arg(long, value_name = "DIR")]
    pub debug_dir: Option<PathBuf>,

    /// Overwrite an existing image with the same image-id instead of returning
    /// an error.
    #[arg(long, default_value_t = false)]
    pub overwrite: bool,
}

pub async fn run(args: CompressArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;

    let storage = config.storage.build().await?;
    let router = config.compressor.build_router()?;

    // Detect image format: explicit flag > file extension.
    let fmt = args
        .image_format
        .as_deref()
        .unwrap_or_else(|| detect_format(&args.image));

    let image_format: Arc<dyn image_delta_core::Image> = match fmt {
        "qcow2" => Arc::new(Qcow2Image::new()),
        _ => Arc::new(DirectoryImage::new()),
    };

    let compressor = DefaultCompressor::new(image_format, Arc::clone(&storage), router);

    // When no base is provided, use an empty temp directory as source so the
    // compressor stores all files as "added" blobs (first-image bootstrap).
    let base_root: &Path = &args.base_image;

    let opts = image_delta_core::CompressOptions {
        image_id: args.image_id.clone(),
        base_image_id: Some(args.base_image_id.clone()),
        workers: args.workers.unwrap_or(config.compressor.workers),
        passthrough_threshold: config.compressor.passthrough_threshold,
        overwrite: args.overwrite,
        debug_dir: args.debug_dir.clone(),
    };

    let stats = compressor.compress(base_root, &args.image, opts).await?;

    let base_label = &args.base_image_id;
    eprintln!(
        "Compressed {} → {}\n  base={}, added={}, patched={}, removed={}, renamed={}, source_bytes={}, stored_bytes={}, elapsed={:.2}s",
        base_label,
        args.image_id,
        base_label,
        stats.files_added,
        stats.files_patched,
        stats.files_removed,
        stats.files_renamed,
        stats.total_source_bytes,
        stats.total_stored_bytes,
        stats.elapsed_secs,
    );
    Ok(())
}

/// Infer image format from file extension or directory-ness.
fn detect_format(path: &Path) -> &'static str {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("qcow2") {
            return "qcow2";
        }
    }
    "directory"
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
