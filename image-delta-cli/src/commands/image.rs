use clap::{Args, Subcommand};

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

pub fn run(_cmd: ImageCommands) -> anyhow::Result<()> {
    todo!("Phase 6: image status / list")
}
