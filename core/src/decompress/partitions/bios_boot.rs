// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress/partitions/bios_boot — BiosBootDecompressor

use async_trait::async_trait;

use crate::decompress::context::DecompressContext;
use crate::decompress::partitions::PartitionDecompressor;
use crate::decompress::PartitionDecompressStats;
use crate::manifest::{PartitionContent, PartitionManifest};
use crate::partitions::PartitionHandle;
use crate::Result;

/// Decompresses a BIOS-boot partition by downloading the verbatim blob and
/// writing it to the output handle via [`write_raw()`][crate::partitions::BiosBootHandle::write_raw].
pub struct BiosBootDecompressor;

#[async_trait]
impl PartitionDecompressor for BiosBootDecompressor {
    async fn decompress(
        &self,
        ctx: &DecompressContext,
        pm: &PartitionManifest,
        output_ph: &PartitionHandle,
    ) -> Result<PartitionDecompressStats> {
        let handle = match output_ph {
            PartitionHandle::BiosBoot(h) => h,
            _ => unreachable!("BiosBootDecompressor called with non-BiosBoot handle"),
        };
        let blob_id = match &pm.content {
            PartitionContent::BiosBoot { blob_id, .. } => *blob_id,
            _ => unreachable!("BiosBootDecompressor called with non-BiosBoot manifest"),
        };
        let data = ctx.storage.download_blob(blob_id).await?;
        let bytes_written = data.len() as u64;
        handle.write_raw(&data)?;
        Ok(PartitionDecompressStats {
            files_written: 1,
            bytes_written,
            patches_verified: 0,
        })
    }
}
