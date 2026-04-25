use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct DecompressArgs {
    /// Image ID to decompress.
    #[arg(long, value_name = "ID")]
    pub image_id: String,

    /// Directory to write the reconstructed filesystem into.
    #[arg(long, value_name = "PATH")]
    pub output: PathBuf,

    /// Number of parallel worker threads (overrides config).
    #[arg(long, value_name = "N")]
    pub workers: Option<usize>,
}

pub fn run(_args: DecompressArgs) -> anyhow::Result<()> {
    todo!("Phase 4: load config → build compressor → decompress")
}
