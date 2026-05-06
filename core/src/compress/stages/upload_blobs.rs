// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 5 — upload_lazy_blobs

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::compress::context::StageContext;
use crate::compress::stage::CompressStage;
use crate::compress::FsDraft;
use crate::manifest::{BlobRef, Data};
use crate::Result;

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 5: upload all `Data::LazyBlob` files to storage, replacing them with
/// `Data::BlobRef`.
///
/// SHA-256 deduplication: if a blob with the same content already exists, the
/// existing UUID is reused and no bytes are transferred.
pub struct UploadLazyBlobs;

#[async_trait]
impl CompressStage for UploadLazyBlobs {
    fn name(&self) -> &'static str {
        "upload_blobs"
    }

    async fn run(&self, ctx: &StageContext, draft: FsDraft) -> Result<FsDraft> {
        upload_lazy_blobs_fn(
            draft,
            ctx.storage.as_ref(),
            &ctx.image_id,
            ctx.base_image_id.as_deref(),
            ctx.partition_number,
        )
        .await
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub async fn upload_lazy_blobs_fn(
    mut draft: FsDraft,
    storage: &dyn crate::storage::Storage,
    image_id: &str,
    base_image_id: Option<&str>,
    partition_number: Option<i32>,
) -> Result<FsDraft> {
    for record in &mut draft.records {
        let lazy_path = match &record.data {
            Some(Data::LazyBlob(p)) => p.clone(),
            _ => continue,
        };

        let bytes = std::fs::read(&lazy_path).map_err(|e| {
            std::io::Error::other(format!(
                "upload_lazy_blobs: cannot read lazy blob '{}': {e}",
                lazy_path.display()
            ))
        })?;
        let sha256_hex = hex_sha256_bytes(&bytes);

        let blob_id = match storage.blob_exists(&sha256_hex).await? {
            Some(id) => {
                debug!(
                    path = %lazy_path.display(),
                    blob_id = %id,
                    "blob already in storage (dedup)"
                );
                id
            }
            None => {
                let id = storage.upload_blob(&sha256_hex, &bytes).await?;
                debug!(
                    path = %lazy_path.display(),
                    blob_id = %id,
                    bytes = bytes.len(),
                    "uploaded blob"
                );
                id
            }
        };
        storage
            .record_blob_origin(
                blob_id,
                image_id,
                base_image_id,
                partition_number,
                record.new_path.as_deref().unwrap_or(""),
            )
            .await?;

        record.data = Some(Data::BlobRef(BlobRef {
            blob_id,
            size: bytes.len() as u64,
        }));
    }
    Ok(draft)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(crate) fn hex_sha256_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}
