// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// image subcommand: list and query registered images

use clap::{Args, Subcommand};
use std::path::Path;
use std::sync::Arc;

use crate::commands::compress::load_config;
use image_delta_core::{Compressor, DefaultCompressor, DeleteOptions, DirectoryImage};

#[derive(Subcommand, Debug)]
pub enum ImageCommands {
    /// Show status of a specific image.
    Status(StatusArgs),
    /// List all images known to storage.
    List(ListArgs),
    /// Delete an image and its exclusively-owned blobs.
    Delete(DeleteArgs),
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Image ID to query.
    #[arg(value_name = "IMAGE_ID")]
    pub image_id: String,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Output format.
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,
}

#[derive(Args, Debug)]
pub struct DeleteArgs {
    /// Image ID to delete.
    #[arg(value_name = "IMAGE_ID")]
    pub image_id: String,

    /// Skip the interactive confirmation prompt.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Print what would be deleted without actually deleting anything.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(cmd: ImageCommands, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;
    let storage = config.storage.build().await?;

    match cmd {
        ImageCommands::Status(args) => match storage.get_image(&args.image_id).await? {
            None => {
                eprintln!("Image '{}' not found", args.image_id);
                std::process::exit(1);
            }
            Some(meta) => {
                println!("image_id:      {}", meta.image_id);
                println!(
                    "base_image_id: {}",
                    meta.base_image_id.as_deref().unwrap_or("-")
                );
                println!("format:        {}", meta.format);
                println!("status:        {}", meta.status);
            }
        },
        ImageCommands::List(args) => {
            let images = storage.list_images().await?;
            if args.format == "json" {
                print!("[");
                for (i, img) in images.iter().enumerate() {
                    if i > 0 {
                        print!(",");
                    }
                    print!(
                        r#"{{"image_id":"{}", "format":"{}", "status":"{}", "base_image_id":{}}}"#,
                        img.image_id,
                        img.format,
                        img.status,
                        img.base_image_id
                            .as_deref()
                            .map(|s| format!("\"{s}\""))
                            .unwrap_or_else(|| "null".into()),
                    );
                }
                println!("]");
            } else {
                println!(
                    "{:<40} {:<12} {:<15} BASE_IMAGE_ID",
                    "IMAGE_ID", "FORMAT", "STATUS"
                );
                for img in &images {
                    println!(
                        "{:<40} {:<12} {:<15} {}",
                        img.image_id,
                        img.format,
                        img.status,
                        img.base_image_id.as_deref().unwrap_or("-"),
                    );
                }
                if images.is_empty() {
                    println!("(no images)");
                }
            }
        }

        ImageCommands::Delete(args) => {
            if !args.yes && !args.dry_run {
                eprint!(
                    "This will permanently delete image '{}' and its exclusive blobs. Confirm? [y/N] ",
                    args.image_id
                );
                use std::io::BufRead;
                let mut line = String::new();
                std::io::BufReader::new(std::io::stdin()).read_line(&mut line)?;
                let answer = line.trim().to_lowercase();
                if answer != "y" && answer != "yes" {
                    eprintln!("Aborted.");
                    std::process::exit(1);
                }
            }

            let router = config.compressor.build_router()?;
            let image_format = Arc::new(DirectoryImage::new());
            let compressor = DefaultCompressor::new(image_format, storage, router);

            let stats = compressor
                .delete_image(DeleteOptions {
                    image_id: args.image_id.clone(),
                    dry_run: args.dry_run,
                })
                .await?;

            if args.dry_run {
                println!(
                    "dry-run: would delete {} blobs, keep {} shared blobs",
                    stats.blobs_deleted, stats.blobs_kept
                );
            } else {
                println!(
                    "deleted image '{}': {} blobs removed, {} blobs kept (shared)",
                    args.image_id, stats.blobs_deleted, stats.blobs_kept
                );
            }
        }
    }
    Ok(())
}
