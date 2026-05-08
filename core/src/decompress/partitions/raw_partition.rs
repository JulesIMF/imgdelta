// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress/partitions/raw_partition — RawPartitionDecompressor

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::decompress::partitions::PartitionDecompressor;
use crate::decompress::PartitionDecompressStats;
use crate::manifest::{PartitionContent, PartitionManifest};
use crate::partitions::PartitionHandle;
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::Result;

/// Decompresses a raw (unformatted) partition by downloading the verbatim blob
/// and writing it to the output handle via [`write_raw()`][crate::partitions::RawHandle::write_raw].
pub struct RawPartitionDecompressor;

#[async_trait]
impl PartitionDecompressor for RawPartitionDecompressor {
    async fn decompress(
        &self,
        pm: &PartitionManifest,
        _base_root: &Path,
        output_ph: &PartitionHandle,
        storage: Arc<dyn Storage>,
        _archive_bytes: &[u8],
        _patches_compressed: bool,
        _router: Arc<RouterEncoder>,
        _workers: usize,
    ) -> Result<PartitionDecompressStats> {
        let handle = match output_ph {
            PartitionHandle::Raw(h) => h,
            _ => unreachable!("RawPartitionDecompressor called with non-Raw handle"),
        };
        let blob_ref = match &pm.content {
            PartitionContent::Raw { blob, .. } => blob.as_ref(),
            _ => unreachable!("RawPartitionDecompressor called with non-Raw manifest"),
        };
        let Some(bref) = blob_ref else {
            // Empty raw partition — nothing to write.
            return Ok(PartitionDecompressStats::default());
        };
        let data = storage.download_blob(bref.blob_id).await?;
        let bytes_written = data.len() as u64;
        handle.write_raw(&data)?;
        Ok(PartitionDecompressStats {
            files_written: 1,
            bytes_written,
            patches_verified: 0,
        })
    }
}
