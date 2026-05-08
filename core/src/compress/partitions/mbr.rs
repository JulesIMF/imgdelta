// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// compress/partitions/mbr — MbrCompressor

use std::collections::HashMap;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::compress::partitions::fs::context::StageContext;
use crate::compress::partitions::PartitionCompressor;
use crate::manifest::{PartitionContent, PartitionManifest};
use crate::partitions::{MountHandle, PartitionHandle};
use crate::Result;

/// Compresses the MBR boot-code area (bytes 0–439) as a single verbatim blob.
pub struct MbrCompressor;

#[async_trait]
impl PartitionCompressor for MbrCompressor {
    async fn compress(
        &self,
        ctx: &StageContext,
        handle: PartitionHandle,
        _fs_type: &str,
        _base_partitions: &HashMap<u32, PartitionHandle>,
        _live_mounts: &mut Vec<Box<dyn MountHandle>>,
        _live_tmpdirs: &mut Vec<tempfile::TempDir>,
    ) -> Result<(PartitionManifest, bool, u64)> {
        let mbr_handle = match handle {
            PartitionHandle::Mbr(h) => h,
            _ => unreachable!("MbrCompressor called with non-Mbr handle"),
        };
        let descriptor = mbr_handle.descriptor.clone();
        let bytes = mbr_handle.read_raw()?;
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let size = bytes.len() as u64;
        let blob_id = match ctx.storage.blob_exists(&sha256).await? {
            Some(id) => id,
            None => ctx.storage.upload_blob(&sha256, &bytes).await?,
        };
        Ok((
            PartitionManifest {
                descriptor,
                content: PartitionContent::MbrBootCode {
                    blob_id,
                    sha256,
                    size,
                },
            },
            false,
            size,
        ))
    }
}
