// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress stage: pre-download all blob refs from the manifest.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::task::JoinSet;
use uuid::Uuid;

use crate::decompress::partitions::fs::context::DecompressContext;
use crate::decompress::partitions::fs::draft::DecompressDraft;
use crate::decompress::partitions::fs::stage::DecompressStage;
use crate::manifest::{Data, EntryType};
use crate::{Error, Result};

/// Pre-download all blobs referenced by manifest records, in parallel.
///
/// Stores the results in `draft.blob_cache` so the subsequent
/// `DeleteRecords`, `RenameRecords`, `ChangeRecords`, `AddRecords` stages
/// can look them up without issuing storage requests individually.
pub struct DownloadBlobs;

#[async_trait::async_trait]
impl DecompressStage for DownloadBlobs {
    fn name(&self) -> &'static str {
        "download_blobs"
    }

    async fn run(
        &self,
        ctx: &DecompressContext,
        mut draft: DecompressDraft,
    ) -> Result<DecompressDraft> {
        let blob_ids: HashSet<Uuid> = ctx
            .records
            .iter()
            .filter(|r| matches!(r.entry_type, EntryType::File))
            .filter_map(|r| {
                if let Some(Data::BlobRef(bref)) = &r.data {
                    Some(bref.blob_id)
                } else {
                    None
                }
            })
            .collect();

        let mut join_set: JoinSet<Result<(Uuid, Vec<u8>)>> = JoinSet::new();
        for id in blob_ids {
            let s = Arc::clone(&ctx.storage);
            join_set.spawn(async move {
                let data = s.download_blob(id).await?;
                Ok((id, data))
            });
        }

        let mut cache: HashMap<Uuid, Vec<u8>> = HashMap::new();
        while let Some(task_result) = join_set.join_next().await {
            let (id, data) = task_result
                .map_err(|e| Error::Other(format!("blob download task panicked: {e}")))??;
            cache.insert(id, data);
        }

        draft.blob_cache = Arc::new(cache);
        Ok(draft)
    }
}
