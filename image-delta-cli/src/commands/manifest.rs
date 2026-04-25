use clap::{Args, Subcommand};

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

pub fn run(_cmd: ManifestCommands) -> anyhow::Result<()> {
    todo!("Phase 6: manifest inspect / diff")
}
