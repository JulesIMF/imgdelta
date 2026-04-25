use clap::Args;
use std::path::PathBuf;

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
}

pub fn run(_args: CompressArgs) -> anyhow::Result<()> {
    todo!("Phase 4: load config → build compressor → compress")
}
