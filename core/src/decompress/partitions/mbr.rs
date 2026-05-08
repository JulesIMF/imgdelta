// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress/partitions/mbr — MbrDecompressor

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

/// Decompresses the MBR boot-code area by downloading the verbatim blob and
/// writing it to the output handle via [`write_raw()`][crate::partitions::MbrHandle::write_raw].
pub struct MbrDecompressor;

#[async_trait]
impl PartitionDecompressor for MbrDecompressor {
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
            PartitionHandle::Mbr(h) => h,
            _ => unreachable!("MbrDecompressor called with non-Mbr handle"),
        };
        let blob_id = match &pm.content {
            PartitionContent::MbrBootCode { blob_id, .. } => *blob_id,
            _ => unreachable!("MbrDecompressor called with non-MbrBootCode manifest"),
        };
        let data = storage.download_blob(blob_id).await?;
        let bytes_written = data.len() as u64;
        handle.write_raw(&data)?;
        Ok(PartitionDecompressStats {
            files_written: 1,
            bytes_written,
            patches_verified: 0,
        })
    }
}
