// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 5 — upload_lazy_blobs

use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tracing::{debug, info};

use crate::compress::partitions::fs::context::StageContext;
use crate::compress::partitions::fs::draft::FsDraft;
use crate::compress::partitions::fs::stage::CompressStage;
use crate::manifest::{BlobRef, Data};
use crate::storage::Storage;
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
            Arc::clone(&ctx.storage),
            &ctx.image_id,
            ctx.base_image_id.as_deref(),
            ctx.partition_number,
            ctx.workers,
        )
        .await
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

/// Per-task input collected before spawning.
struct LazyEntry {
    record_idx: usize,
    lazy_path: std::path::PathBuf,
    new_path: String,
    precomputed_hash: Option<[u8; 32]>,
}

/// Per-SHA-256 group built after phase 1.
struct Sha256Group {
    /// Blob UUID if `blob_exists` returned `Some` for at least one record.
    existing_id: Option<uuid::Uuid>,
    /// All (record_idx, new_path) belonging to this SHA-256.
    records: Vec<(usize, String)>,
    /// Path to any representative file for this SHA-256 (used for upload).
    representative_path: std::path::PathBuf,
}

pub async fn upload_lazy_blobs_fn(
    mut draft: FsDraft,
    storage: Arc<dyn Storage>,
    image_id: &str,
    base_image_id: Option<&str>,
    partition_number: Option<i32>,
    workers: usize,
) -> Result<FsDraft> {
    // Collect all lazy-blob records.
    let entries: Vec<LazyEntry> = draft
        .records
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match &r.data {
            Some(Data::LazyBlob(p)) => Some(LazyEntry {
                record_idx: i,
                new_path: r.new_path.as_deref().unwrap_or("").to_string(),
                precomputed_hash: draft.blob_sha256.get(p).copied(),
                lazy_path: p.clone(),
            }),
            _ => None,
        })
        .collect();

    let total = entries.len();
    if total == 0 {
        info!(uploaded = 0, deduped = 0, "upload_blobs: done");
        return Ok(draft);
    }

    let workers = workers.max(1);

    // ── Phase 1: compute SHA-256 + call blob_exists for every entry ───────────
    //
    // Each file gets its own `blob_exists` call (so the call count equals the
    // number of files).  After collecting *all* results we know which sha256
    // values are absent from storage; only then do we deduplicate within the
    // batch and launch uploads.  This eliminates the TOCTOU race that would
    // arise if we spawned uploads immediately after each `blob_exists`.

    let sem = Arc::new(Semaphore::new(workers));
    let mut phase1_join: tokio::task::JoinSet<Result<(usize, String, Option<uuid::Uuid>)>> =
        tokio::task::JoinSet::new();

    for entry in &entries {
        let sem = Arc::clone(&sem);
        let storage = Arc::clone(&storage);
        let path = entry.lazy_path.clone();
        let precomputed = entry.precomputed_hash;
        let idx = entry.record_idx;

        phase1_join.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore closed");

            let sha256 = if let Some(h) = precomputed {
                hex_sha256_bytes_raw(&h)
            } else {
                let path_str = path.display().to_string();
                let bytes = tokio::task::spawn_blocking(move || std::fs::read(&path))
                    .await
                    .map_err(|e| crate::Error::Other(format!("hash task panicked: {e}")))?
                    .map_err(|e| crate::Error::Other(format!("read for hash '{path_str}': {e}")))?;
                hex_sha256_bytes(&bytes)
            };

            let existing = storage.blob_exists(&sha256).await?;
            Ok((idx, sha256, existing))
        });
    }

    // Build sha256 → group map from phase 1 results.
    // Build a lookup from record_idx to &LazyEntry.
    let idx_to_entry: std::collections::HashMap<usize, &LazyEntry> =
        entries.iter().map(|e| (e.record_idx, e)).collect();

    let mut groups: std::collections::HashMap<String, Sha256Group> =
        std::collections::HashMap::new();

    while let Some(res) = phase1_join.join_next().await {
        let (idx, sha256, existing) =
            res.map_err(|e| crate::Error::Other(format!("phase1 task panicked: {e}")))??;
        let entry = idx_to_entry[&idx];
        let group = groups.entry(sha256).or_insert_with(|| Sha256Group {
            existing_id: None,
            records: Vec::new(),
            representative_path: entry.lazy_path.clone(),
        });
        if let Some(id) = existing {
            group.existing_id = Some(id);
        }
        group.records.push((idx, entry.new_path.clone()));
    }

    // ── Phase 2: upload one task per unique SHA-256 absent from storage ───────

    let image_id_arc: Arc<str> = Arc::from(image_id);
    let base_image_id_arc: Option<Arc<str>> = base_image_id.map(Arc::from);

    let sem = Arc::new(Semaphore::new(workers));
    let mut upload_join: tokio::task::JoinSet<Result<(String, uuid::Uuid)>> =
        tokio::task::JoinSet::new();

    for (sha256_hex, group) in &groups {
        if group.existing_id.is_some() {
            continue; // already in storage — skip upload
        }
        let sem = Arc::clone(&sem);
        let storage = Arc::clone(&storage);
        let sha256_hex = sha256_hex.clone();
        let path = group.representative_path.clone();

        upload_join.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore closed");
            let path_str = path.display().to_string();
            let p = path.clone();
            let bytes = tokio::task::spawn_blocking(move || std::fs::read(&p))
                .await
                .map_err(|e| crate::Error::Other(format!("upload task panicked: {e}")))?
                .map_err(|e| crate::Error::Other(format!("read blob '{path_str}': {e}")))?;
            let id = storage.upload_blob(&sha256_hex, &bytes).await?;
            debug!(blob_id = %id, bytes = bytes.len(), "uploaded blob");
            Ok((sha256_hex, id))
        });
    }

    // Finalise sha256 → blob_id map.
    let mut sha256_to_id: std::collections::HashMap<String, uuid::Uuid> =
        std::collections::HashMap::with_capacity(groups.len());
    for (sha256, group) in &groups {
        if let Some(id) = group.existing_id {
            sha256_to_id.insert(sha256.clone(), id);
        }
    }

    let mut uploaded = 0usize;
    let mut blobs_stored_bytes = 0u64;
    while let Some(res) = upload_join.join_next().await {
        let (sha256, id) =
            res.map_err(|e| crate::Error::Other(format!("upload task panicked: {e}")))??;
        // Count bytes for newly uploaded (non-deduped) blobs.
        if let Some(group) = groups.get(&sha256) {
            blobs_stored_bytes += group
                .representative_path
                .metadata()
                .map(|m| m.len())
                .unwrap_or(0);
        }
        sha256_to_id.insert(sha256, id);
        uploaded += 1;
    }

    // ── Phase 3: record origins and apply BlobRefs to draft ───────────────────

    for (sha256_hex, group) in &groups {
        let blob_id = sha256_to_id[sha256_hex];
        let size = group
            .representative_path
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        let blob_ref = BlobRef { blob_id, size };

        for (record_idx, new_path) in &group.records {
            storage
                .record_blob_origin(
                    blob_id,
                    image_id_arc.as_ref(),
                    base_image_id_arc.as_deref(),
                    partition_number,
                    new_path,
                )
                .await?;
            draft.records[*record_idx].data = Some(Data::BlobRef(blob_ref.clone()));
        }
    }

    let deduped = total - uploaded;
    info!(
        total,
        uploaded, deduped, blobs_stored_bytes, workers, "upload_blobs: done"
    );
    draft.blobs_stored_bytes += blobs_stored_bytes;
    Ok(draft)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(crate) fn hex_sha256_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Encode a pre-computed raw SHA-256 digest as a hex string.
fn hex_sha256_bytes_raw(hash: &[u8; 32]) -> String {
    hex::encode(hash)
}
