// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// image subcommand: list and query registered images

use clap::{Args, Subcommand};
use std::path::Path;

use crate::commands::compress::load_config;

#[derive(Subcommand, Debug)]
pub enum ImageCommands {
    /// Show status of a specific image.
    Status(StatusArgs),
    /// List all images known to storage.
    List(ListArgs),
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
    }
    Ok(())
}
