// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress/partitions/fs — FS partition decompressor and pipeline entry point

pub mod context;
pub mod draft;
pub mod pipeline;
pub mod stage;
pub mod stages;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::decompress::PartitionDecompressStats;
use crate::manifest::{PartitionContent, PartitionManifest, Record};
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::Result;

use context::DecompressContext;
use draft::DecompressDraft;
use pipeline::DecompressPipeline;

pub use crate::decompress::partitions::PartitionDecompressor;

// ── FsPartitionDecompressor ───────────────────────────────────────────────────

/// Decompresses an Fs partition by running the 3-stage decompress pipeline.
pub struct FsPartitionDecompressor;

#[async_trait]
impl PartitionDecompressor for FsPartitionDecompressor {
    async fn decompress(
        &self,
        pm: &PartitionManifest,
        base_root: &Path,
        output_root: &Path,
        storage: Arc<dyn Storage>,
        archive_bytes: &[u8],
        patches_compressed: bool,
        router: Arc<RouterEncoder>,
        workers: usize,
    ) -> Result<PartitionDecompressStats> {
        let records = match &pm.content {
            PartitionContent::Fs { records, .. } => records,
            _ => unreachable!("FsPartitionDecompressor called with non-Fs partition"),
        };

        decompress_fs_partition(
            base_root,
            output_root,
            records,
            archive_bytes,
            patches_compressed,
            storage,
            router,
            workers,
        )
        .await
    }
}

// ── decompress_fs_partition ───────────────────────────────────────────────────

/// Reconstruct an Fs partition into `output_root` from `base_root` + manifest records.
#[allow(clippy::too_many_arguments)]
pub async fn decompress_fs_partition(
    base_root: &Path,
    output_root: &Path,
    records: &[Record],
    archive_bytes: &[u8],
    patches_compressed: bool,
    storage: Arc<dyn Storage>,
    router: Arc<RouterEncoder>,
    workers: usize,
) -> Result<PartitionDecompressStats> {
    let ctx = DecompressContext {
        storage,
        router,
        workers,
        base_root: Arc::from(base_root),
        output_root: Arc::from(output_root),
        records: Arc::from(records),
        archive_bytes: Arc::from(archive_bytes),
        patches_compressed,
    };

    let pipeline = DecompressPipeline::default_fs();
    let draft = pipeline.run(&ctx, DecompressDraft::default()).await?;

    Ok(draft.stats)
}
