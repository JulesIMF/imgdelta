use clap::{Args, Subcommand};
use std::path::Path;

use image_delta_core::Manifest;

use crate::commands::compress::load_config;

#[derive(Subcommand, Debug)]
pub enum ManifestCommands {
    /// Display manifest contents for an image.
    Inspect(InspectArgs),
    /// Compare manifests of two images.
    Diff(DiffArgs),
}

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// Image ID whose manifest to display.
    #[arg(value_name = "IMAGE_ID")]
    pub image_id: String,

    /// Output format.
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,
}

#[derive(Args, Debug)]
pub struct DiffArgs {
    /// Base image ID.
    #[arg(value_name = "BASE_IMAGE_ID")]
    pub base_id: String,

    /// Target image ID.
    #[arg(value_name = "TARGET_IMAGE_ID")]
    pub target_id: String,
}

pub async fn run(cmd: ManifestCommands, config_path: Option<&Path>) -> anyhow::Result<()> {
    match cmd {
        ManifestCommands::Inspect(args) => inspect(args, config_path).await,
        ManifestCommands::Diff(_) => {
            anyhow::bail!("manifest diff is not yet implemented")
        }
    }
}

async fn inspect(args: InspectArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;
    let storage = config.storage.build().await?;

    let bytes = storage
        .download_manifest(&args.image_id)
        .await
        .map_err(|e| anyhow::anyhow!("download_manifest '{}': {e}", args.image_id))?;

    let manifest = Manifest::from_bytes(&bytes)
        .map_err(|e| anyhow::anyhow!("parse manifest '{}': {e}", args.image_id))?;

    if args.format == "json" {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
        return Ok(());
    }

    // Table format
    let h = &manifest.header;
    println!("image_id:      {}", h.image_id);
    println!(
        "base_image_id: {}",
        h.base_image_id.as_deref().unwrap_or("-")
    );
    println!("format:        {}", h.format);
    println!("version:       {}", h.version);
    println!("partitions:    {}", manifest.partitions.len());
    println!();
    for pm in &manifest.partitions {
        println!(
            "  partition {} ({:?})",
            pm.descriptor.number, pm.descriptor.kind
        );
    }
    Ok(())
}
