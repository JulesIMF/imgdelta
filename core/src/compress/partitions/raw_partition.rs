// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// compress/partitions/raw_partition — RawPartitionCompressor

use std::collections::HashMap;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::compress::partitions::fs::context::StageContext;
use crate::compress::partitions::PartitionCompressor;
use crate::manifest::{BlobRef, PartitionContent, PartitionManifest};
use crate::partitions::{MountHandle, PartitionHandle};
use crate::Result;

/// Compresses a raw partition by uploading the raw bytes as a single blob.
pub struct RawPartitionCompressor;

#[async_trait]
impl PartitionCompressor for RawPartitionCompressor {
    async fn compress(
        &self,
        ctx: &StageContext,
        handle: PartitionHandle,
        _fs_type: &str,
        _base_partitions: &HashMap<u32, PartitionHandle>,
        _live_mounts: &mut Vec<Box<dyn MountHandle>>,
        _live_tmpdirs: &mut Vec<tempfile::TempDir>,
    ) -> Result<(PartitionManifest, bool, u64)> {
        let raw_handle = match handle {
            PartitionHandle::Raw(h) => h,
            _ => unreachable!("RawPartitionCompressor called with non-Raw handle"),
        };
        let descriptor = raw_handle.descriptor.clone();
        let bytes = raw_handle.read_raw()?;
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let size = bytes.len() as u64;
        let blob_id = match ctx.storage.blob_exists(&sha256).await? {
            Some(id) => id,
            None => ctx.storage.upload_blob(&sha256, &bytes).await?,
        };
        Ok((
            PartitionManifest {
                descriptor,
                content: PartitionContent::Raw {
                    size,
                    blob: Some(BlobRef { blob_id, size }),
                    patch: None,
                },
            },
            false,
            size,
        ))
    }
}
