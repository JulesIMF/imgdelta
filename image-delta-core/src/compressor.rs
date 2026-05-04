use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::manifest::{BlobRef, Entry, EntryType, Manifest, ManifestHeader, Metadata, PatchRef};
use crate::path_match::{find_best_matches, PathMatchConfig};
use crate::routing::RouterEncoder;
use crate::storage::ImageMeta;
use crate::{FileSnapshot, PatchEncoder, Result, Storage};

// ── Stats ─────────────────────────────────────────────────────────────────────

/// Statistics produced by a compress operation.
#[derive(Debug, Clone, Default)]
pub struct CompressionStats {
    /// Files encoded as VCDIFF patches against a base file.
    pub files_patched: usize,
    /// Files stored as verbatim blobs (new files or passthrough fallback).
    pub files_added: usize,
    /// Files marked as removed in the manifest.
    pub files_removed: usize,
    /// Total uncompressed size of source files that were patched or added.
    pub total_source_bytes: u64,
    /// Total bytes uploaded to storage (patches + verbatim blobs).
    pub total_stored_bytes: u64,
    /// Wall-clock time taken by the compress operation, in seconds.
    pub elapsed_secs: f64,
}

impl CompressionStats {
    /// Compression ratio: stored / source.  Lower is better.
    pub fn ratio(&self) -> f64 {
        if self.total_source_bytes == 0 {
            return 1.0;
        }
        self.total_stored_bytes as f64 / self.total_source_bytes as f64
    }
}

/// Statistics produced by a decompress operation.
#[derive(Debug, Clone, Default)]
pub struct DecompressionStats {
    /// Total number of entries reconstructed (added + patched + metadata-only).
    pub total_files: usize,
    /// Number of patch archive entries whose SHA-256 was verified successfully.
    pub patches_verified: usize,
    /// Total bytes written to the output directory.
    pub total_bytes: u64,
    /// Wall-clock time taken by the decompress operation, in seconds.
    pub elapsed_secs: f64,
}

// ── Options ───────────────────────────────────────────────────────────────────

/// Options for a compress operation.
pub struct CompressOptions {
    /// Provider-assigned identifier for the image being compressed.
    pub image_id: String,
    /// Provider-assigned identifier for the base image.
    pub base_image_id: Option<String>,
    /// Number of parallel worker threads (reserved; Phase 4 runs single-threaded).
    pub workers: usize,
    /// Fall back to verbatim blob when `delta_size >= source_size * threshold`.
    /// Set to `1.0` to always prefer the delta when it is any smaller.
    pub passthrough_threshold: f64,
}

/// Options for a decompress operation.
pub struct DecompressOptions {
    /// Provider-assigned identifier of the image to reconstruct.
    pub image_id: String,
    /// Path to the base image filesystem root used during compression.
    pub base_root: PathBuf,
    /// Number of parallel worker threads (reserved).
    pub workers: usize,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// High-level compress/decompress operations.
#[async_trait]
pub trait Compressor: Send + Sync {
    /// Compress `target_root` relative to `source_root` and store the manifest
    /// and patches via the [`Storage`] backend.
    async fn compress(
        &self,
        source_root: &Path,
        target_root: &Path,
        options: CompressOptions,
    ) -> Result<CompressionStats>;

    /// Download patches from storage and reconstruct the image at `output_root`.
    async fn decompress(
        &self,
        output_root: &Path,
        options: DecompressOptions,
    ) -> Result<DecompressionStats>;
}

// ── DefaultCompressor ─────────────────────────────────────────────────────────

/// Production [`Compressor`] implementation.
///
/// Owns a [`Storage`] backend and a [`RouterEncoder`] for per-file encoder
/// selection.  For single-encoder use, construct with [`DefaultCompressor::with_encoder`].
pub struct DefaultCompressor {
    storage: Arc<dyn Storage>,
    router: Arc<RouterEncoder>,
}

impl DefaultCompressor {
    /// Create a new `DefaultCompressor` backed by the given storage and router.
    pub fn new(storage: Arc<dyn Storage>, router: Arc<RouterEncoder>) -> Self {
        Self { storage, router }
    }

    /// Convenience constructor for the common case of a single encoder without
    /// per-file routing rules.
    pub fn with_encoder(storage: Arc<dyn Storage>, encoder: Arc<dyn PatchEncoder>) -> Self {
        Self::new(storage, Arc::new(RouterEncoder::new(vec![], encoder)))
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn create_parent_dirs(p: &Path) -> std::io::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Build a tar archive from a list of `(entry_name, data)` pairs.
fn build_patches_tar(patches: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in patches {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, data.as_slice())
            .map_err(crate::Error::Io)?;
    }
    builder.finish().map_err(crate::Error::Io)?;
    builder.into_inner().map_err(crate::Error::Io)
}

/// Extract a tar archive into a `HashMap<name, bytes>`.
fn extract_tar(data: &[u8]) -> Result<HashMap<String, Vec<u8>>> {
    let cursor = std::io::Cursor::new(data);
    let mut archive = tar::Archive::new(cursor);
    let mut map = HashMap::new();
    for entry in archive.entries().map_err(crate::Error::Io)? {
        let mut entry = entry.map_err(crate::Error::Io)?;
        let name = entry
            .path()
            .map_err(crate::Error::Io)?
            .to_string_lossy()
            .into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(crate::Error::Io)?;
        map.insert(name, bytes);
    }
    Ok(map)
}

/// Recursively copy `src` to `dst`, preserving symlinks without following them.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    for e in WalkDir::new(src).follow_links(false) {
        let e = e.map_err(|err| crate::Error::Io(err.into()))?;
        let rel = e
            .path()
            .strip_prefix(src)
            .expect("strip_prefix succeeds when walking a subtree");
        let dst_path = dst.join(rel);
        if e.file_type().is_dir() {
            std::fs::create_dir_all(&dst_path)?;
        } else if e.file_type().is_symlink() {
            let link_target = std::fs::read_link(e.path())?;
            if dst_path.symlink_metadata().is_ok() {
                std::fs::remove_file(&dst_path)?;
            }
            std::os::unix::fs::symlink(link_target, &dst_path)?;
        } else {
            create_parent_dirs(&dst_path)?;
            std::fs::copy(e.path(), &dst_path)?;
        }
    }
    Ok(())
}

/// Build a `(dev, ino) → canonical_path` map for regular files in `root`.
///
/// The canonical path for each inode group is the **lexicographically smallest**
/// relative path that shares that inode.  This is deterministic regardless of
/// WalkDir traversal order, and ensures that a new file whose inode is shared
/// with an existing (unchanged) file is correctly identified as a hardlink.
fn build_inode_map(root: &Path) -> Result<HashMap<(u64, u64), String>> {
    let mut map: HashMap<(u64, u64), String> = HashMap::new();
    for e in WalkDir::new(root).follow_links(false) {
        let e = e.map_err(|err| crate::Error::Io(err.into()))?;
        if !e.file_type().is_file() {
            continue;
        }
        let meta = e.path().symlink_metadata()?;
        let key = (meta.dev(), meta.ino());
        let rel = e
            .path()
            .strip_prefix(root)
            .expect("strip_prefix succeeds")
            .to_string_lossy()
            .replace('\\', "/");
        let entry = map.entry(key).or_insert_with(|| rel.clone());
        if rel < *entry {
            *entry = rel;
        }
    }
    Ok(map)
}

/// Return only the metadata fields that differ between `source_path` and `target_path`.
fn collect_metadata_changes(source_path: &Path, target_path: &Path) -> Result<Metadata> {
    let s = source_path.symlink_metadata()?;
    let t = target_path.symlink_metadata()?;
    Ok(Metadata {
        mode: {
            let sm = s.mode() & 0o7777;
            let tm = t.mode() & 0o7777;
            if sm != tm {
                Some(tm)
            } else {
                None
            }
        },
        uid: if s.uid() != t.uid() {
            Some(t.uid())
        } else {
            None
        },
        gid: if s.gid() != t.gid() {
            Some(t.gid())
        } else {
            None
        },
        mtime: {
            if s.mtime().abs_diff(t.mtime()) > 1 {
                Some(t.mtime())
            } else {
                None
            }
        },
        ..Default::default()
    })
}

/// Apply `mode` and `mtime` metadata to `root/path`.  Renames are Phase 2.
fn apply_metadata(root: &Path, path: &str, meta: &Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let p = root.join(path);
    let file_type = match p.symlink_metadata() {
        Ok(m) => m.file_type(),
        Err(_) => return Ok(()), // file may have been renamed already
    };
    if let Some(mode) = meta.mode {
        if !file_type.is_symlink() {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode))?;
        }
    }
    if let Some(mtime) = meta.mtime {
        if !file_type.is_symlink() {
            filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(mtime, 0))?;
        }
    }
    Ok(())
}

fn entry_type_of(path: &Path) -> EntryType {
    match path.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => EntryType::Symlink,
        Ok(m) if m.file_type().is_dir() => EntryType::Directory,
        Ok(_) => EntryType::File,
        Err(_) => EntryType::Other,
    }
}

// ── Compress impl ─────────────────────────────────────────────────────────────

// ── Phase 2 types: parallel encoding ─────────────────────────────────────────

/// Work unit for the rayon parallel encoding phase (Phase 2 of compress).
/// All bytes are pre-loaded from disk in Phase 1.
struct EncodeTask {
    /// Path that goes in `entry.path` — source_path for rename+change, same path for plain change.
    path: String,
    /// New target path for rename+change entries (`entry.metadata.new_path`).
    rename_to: Option<String>,
    source_bytes: Vec<u8>,
    target_bytes: Vec<u8>,
    target_size: u64,
    passthrough_threshold: f64,
    /// Extra metadata (mode, uid, gid, mtime) gathered in Phase 1.
    extra_metadata: Option<Metadata>,
    /// If `Some`, this is a BlobPatch task: `source_bytes` came from a blob stored
    /// under the base image.  The `BlobRef` becomes `entry.blob` in the manifest
    /// so that decompress knows where to fetch the base bytes for delta decoding.
    base_blob_ref: Option<BlobRef>,
}

/// How the rayon encoding phase resolved a single file.
enum EncodeOutcome {
    /// A delta patch that fits within the passthrough threshold.
    Patch {
        archive_entry: String,
        patch_bytes: Vec<u8>,
        sha256: String,
        code: crate::AlgorithmCode,
        algorithm_id: Option<String>,
        /// Carried from `EncodeTask::base_blob_ref`.  `Some` means this patch is a
        /// BlobPatch; the referenced blob must appear as `entry.blob` in the manifest.
        base_blob_ref: Option<BlobRef>,
    },
    /// Delta too large; store the target verbatim (passthrough fallback).
    Passthrough {
        target_bytes: Vec<u8>,
        sha256: String,
    },
}

/// Result returned by `encode_one` for each `EncodeTask`.
struct EncodeResult {
    path: String,
    rename_to: Option<String>,
    target_size: u64,
    outcome: EncodeOutcome,
    extra_metadata: Option<Metadata>,
}

/// A regular file added fresh (no delta needed) — blob upload deferred to Phase 3.
struct DeferredBlob {
    path: String,
    bytes: Vec<u8>,
    sha256: String,
    size: u64,
}

// ── encode_one — called from rayon thread pool ────────────────────────────────

/// Encode a single file delta.  Called from rayon workers (Phase 2).
///
/// This function is pure CPU work: no I/O, no async, no allocation beyond
/// the produced patch bytes.
fn encode_one(task: EncodeTask, router: &RouterEncoder) -> Result<EncodeResult> {
    let target_path = task.rename_to.as_deref().unwrap_or(&task.path);
    let base_snap = FileSnapshot {
        path: &task.path,
        size: task.source_bytes.len() as u64,
        header: &task.source_bytes[..task.source_bytes.len().min(16)],
        bytes: &task.source_bytes,
    };
    let target_snap = FileSnapshot {
        path: target_path,
        size: task.target_size,
        header: &task.target_bytes[..task.target_bytes.len().min(16)],
        bytes: &task.target_bytes,
    };
    let file_patch = router.encode(&base_snap, &target_snap)?;
    let use_patch = (file_patch.bytes.len() as f64)
        < (task.target_bytes.len() as f64) * task.passthrough_threshold;

    let outcome = if use_patch {
        let archive_entry = format!("{}.patch", uuid::Uuid::new_v4());
        let sha256 = sha256_hex(&file_patch.bytes);
        EncodeOutcome::Patch {
            archive_entry,
            patch_bytes: file_patch.bytes,
            sha256,
            code: file_patch.code,
            algorithm_id: file_patch.algorithm_id,
            base_blob_ref: task.base_blob_ref,
        }
    } else {
        let sha256 = sha256_hex(&task.target_bytes);
        EncodeOutcome::Passthrough {
            target_bytes: task.target_bytes,
            sha256,
        }
    };

    Ok(EncodeResult {
        path: task.path,
        rename_to: task.rename_to,
        target_size: task.target_size,
        outcome,
        extra_metadata: task.extra_metadata,
    })
}

// ── Compress impl ─────────────────────────────────────────────────────────────

#[async_trait]
impl Compressor for DefaultCompressor {
    /// Compress `target_root` relative to `source_root` in three phases:
    ///
    /// **Phase 1** (sequential, disk I/O): diff, rename-match, classify entries,
    /// read file bytes, build `EncodeTask` and `DeferredBlob` queues.
    ///
    /// **Phase 2** (rayon parallel, CPU): delta-encode each `EncodeTask` in a
    /// dedicated thread pool.  NOTE: this blocks the current async thread.
    /// Phase 5.4 will move encoding into `tokio::task::spawn_blocking`.
    ///
    /// **Phase 3** (sequential, async I/O): upload blobs and patches, build
    /// and upload manifest, register image metadata.
    async fn compress(
        &self,
        source_root: &Path,
        target_root: &Path,
        options: CompressOptions,
    ) -> Result<CompressionStats> {
        use crate::fs_diff::{diff_dirs, DiffKind};
        use std::time::{SystemTime, UNIX_EPOCH};

        let mut stats = CompressionStats::default();
        let start = std::time::Instant::now();

        // ── Pre-diff work ─────────────────────────────────────────────────────
        let diff = diff_dirs(source_root, target_root)?;

        let removed_paths: Vec<String> = diff.removed().map(|d| d.path.clone()).collect();
        let added_paths: Vec<String> = diff.added().map(|d| d.path.clone()).collect();
        let renames = find_best_matches(&removed_paths, &added_paths, &PathMatchConfig::default())?;

        let renamed_sources: HashSet<String> =
            renames.iter().map(|r| r.source_path.clone()).collect();
        let renamed_targets: HashMap<String, String> = renames
            .iter()
            .map(|r| (r.target_path.clone(), r.source_path.clone()))
            .collect();

        let inode_map = build_inode_map(target_root)?;

        // ─────────────────────────────────────────────────────────────────────
        // Phase 1: classify entries, read file bytes into task queues
        // ─────────────────────────────────────────────────────────────────────

        let mut immediate_entries: Vec<Entry> = Vec::new();
        let mut encode_tasks: Vec<EncodeTask> = Vec::new();
        let mut deferred_blobs: Vec<DeferredBlob> = Vec::new();

        // — BlobPatch candidate matching —
        //
        // For each regular file that is *added* in the target (not a rename target),
        // check whether any blob stored under the base image makes a good delta base.
        // Matched files become `EncodeTask`s with `base_blob_ref = Some(...)` so that
        // Phase 2 encodes a patch against the downloaded blob bytes instead of an
        // identical source file.
        //
        // This is how "cross-image" delta encoding works: if libfoo.so.1 was stored as
        // a blob in the base image, libfoo.so.2 added in the current image can be
        // stored as a tiny patch rather than a full copy.
        let mut blob_patch_paths: HashSet<String> = HashSet::new();
        if let Some(base_id) = &options.base_image_id {
            let candidates = self.storage.find_blob_candidates(base_id).await?;
            if !candidates.is_empty() {
                use crate::fs_diff::DiffKind;

                // Paths being handled as Changed entries already have their own source;
                // they don't need an additional blob base.
                let changed_paths: HashSet<&str> = diff
                    .diffs
                    .iter()
                    .filter(|d| d.kind == DiffKind::Changed)
                    .map(|d| d.path.as_str())
                    .collect();

                let valid_candidates: Vec<_> = candidates
                    .iter()
                    .filter(|c| !changed_paths.contains(c.original_path.as_str()))
                    .collect();

                // Only regular files that are freshly added (not rename targets).
                let added_file_paths: Vec<String> = diff
                    .added()
                    .filter(|d| !renamed_targets.contains_key(&d.path))
                    .filter(|d| {
                        target_root
                            .join(&d.path)
                            .symlink_metadata()
                            .map(|m| m.file_type().is_file())
                            .unwrap_or(false)
                    })
                    .map(|d| d.path.clone())
                    .collect();

                if !added_file_paths.is_empty() && !valid_candidates.is_empty() {
                    let candidate_paths: Vec<String> = valid_candidates
                        .iter()
                        .map(|c| c.original_path.clone())
                        .collect();
                    let blob_matches = find_best_matches(
                        &candidate_paths,
                        &added_file_paths,
                        &PathMatchConfig::default(),
                    )?;

                    for m in &blob_matches {
                        let candidate = valid_candidates
                            .iter()
                            .find(|c| c.original_path == m.source_path)
                            .expect("match source_path must correspond to a valid candidate");
                        let blob_bytes = self.storage.download_blob(candidate.uuid).await?;
                        let target_file = target_root.join(&m.target_path);
                        let target_bytes = std::fs::read(&target_file)?;
                        let blob_size = blob_bytes.len() as u64;
                        let target_size = target_bytes.len() as u64;
                        stats.total_source_bytes += target_size;
                        encode_tasks.push(EncodeTask {
                            path: m.target_path.clone(),
                            rename_to: None,
                            source_bytes: blob_bytes,
                            target_bytes,
                            target_size,
                            passthrough_threshold: options.passthrough_threshold,
                            extra_metadata: None,
                            base_blob_ref: Some(BlobRef {
                                blob_id: candidate.uuid,
                                size: blob_size,
                            }),
                        });
                        blob_patch_paths.insert(m.target_path.clone());
                    }
                }
            }
        }

        // — Removed (not renamed) —
        for d in diff.removed() {
            if renamed_sources.contains(&d.path) {
                continue;
            }
            immediate_entries.push(Entry {
                path: d.path.clone(),
                entry_type: EntryType::File,
                size: 0,
                blob: None,
                patch: None,
                metadata: None,
                hardlink_target: None,
                removed: true,
            });
            stats.files_removed += 1;
        }

        // — Added (not target of a rename, not a BlobPatch candidate) —
        for d in diff.added() {
            if renamed_targets.contains_key(&d.path) {
                continue;
            }
            if blob_patch_paths.contains(&d.path) {
                continue;
            }
            let target_file = target_root.join(&d.path);
            self.classify_added_entry(
                &target_file,
                &d.path,
                &inode_map,
                &mut immediate_entries,
                &mut deferred_blobs,
                &mut stats,
            )?;
        }

        // — Renames —
        for rename in &renames {
            let source_file = source_root.join(&rename.source_path);
            let target_file = target_root.join(&rename.target_path);
            self.classify_rename_entry(
                &source_file,
                &target_file,
                &rename.source_path,
                &rename.target_path,
                options.passthrough_threshold,
                &mut immediate_entries,
                &mut encode_tasks,
                &mut stats,
            )?;
        }

        // — Changed —
        for d in diff.diffs.iter().filter(|d| d.kind == DiffKind::Changed) {
            let source_file = source_root.join(&d.path);
            let target_file = target_root.join(&d.path);
            self.classify_changed_entry(
                &source_file,
                &target_file,
                &d.path,
                options.passthrough_threshold,
                &mut immediate_entries,
                &mut encode_tasks,
                &mut stats,
            )?;
        }

        // — MetadataOnly —
        for d in diff.metadata_only() {
            let source_file = source_root.join(&d.path);
            let target_file = target_root.join(&d.path);
            let meta = collect_metadata_changes(&source_file, &target_file)?;
            let has_changes = meta.mode.is_some()
                || meta.uid.is_some()
                || meta.gid.is_some()
                || meta.mtime.is_some();
            immediate_entries.push(Entry {
                path: d.path.clone(),
                entry_type: entry_type_of(&target_file),
                size: 0,
                blob: None,
                patch: None,
                metadata: if has_changes { Some(meta) } else { None },
                hardlink_target: None,
                removed: false,
            });
        }

        // ─────────────────────────────────────────────────────────────────────
        // Phase 2: parallel encoding via rayon
        //
        // NOTE: `pool.install` blocks the current thread for the duration of
        // the encoding work.  This is acceptable in tests (single-thread tokio
        // executor) and for moderate workloads.  Phase 5.4 will move this
        // block into `tokio::task::spawn_blocking` for production use.
        // ─────────────────────────────────────────────────────────────────────

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(options.workers.max(1))
            .build()
            .map_err(|e| crate::Error::Other(format!("rayon thread pool: {e}")))?;

        let router = Arc::clone(&self.router);
        let raw_results: Vec<Result<EncodeResult>> = pool.install(|| {
            encode_tasks
                .into_par_iter()
                .map(|t| encode_one(t, &router))
                .collect()
        });
        let encode_results: Vec<EncodeResult> =
            raw_results.into_iter().collect::<Result<Vec<_>>>()?;

        // ─────────────────────────────────────────────────────────────────────
        // Phase 3: sequential async — upload blobs and patches, build manifest
        // ─────────────────────────────────────────────────────────────────────

        let mut entries = immediate_entries;
        let mut patches: Vec<(String, Vec<u8>)> = Vec::new();

        // Upload deferred blobs (added regular files — no encoding needed).
        for db in deferred_blobs {
            let blob_id = self.storage.upload_blob(&db.sha256, &db.bytes).await?;
            self.storage
                .record_blob_origin(
                    blob_id,
                    &options.image_id,
                    options.base_image_id.as_deref(),
                    &db.path,
                )
                .await?;
            stats.total_stored_bytes += db.size;
            entries.push(Entry {
                path: db.path,
                entry_type: EntryType::File,
                size: db.size,
                blob: Some(BlobRef {
                    blob_id,
                    size: db.size,
                }),
                patch: None,
                metadata: None,
                hardlink_target: None,
                removed: false,
            });
        }

        // Process encode results: collect patches or upload passthrough blobs.
        for result in encode_results {
            let entry = match result.outcome {
                EncodeOutcome::Patch {
                    archive_entry,
                    patch_bytes,
                    sha256,
                    code,
                    algorithm_id,
                    base_blob_ref,
                } => {
                    stats.total_stored_bytes += patch_bytes.len() as u64;
                    stats.files_patched += 1;
                    patches.push((archive_entry.clone(), patch_bytes));
                    // Build metadata: carry rename new_path + any extra metadata.
                    let metadata = if result.rename_to.is_some() || result.extra_metadata.is_some()
                    {
                        let mut m = result.extra_metadata.unwrap_or_default();
                        m.new_path = result.rename_to;
                        Some(m)
                    } else {
                        None
                    };
                    Entry {
                        path: result.path,
                        entry_type: EntryType::File,
                        size: result.target_size,
                        // BlobPatch: blob = base blob from previous image, patch = delta.
                        // Regular patch: blob = None (base is the source filesystem).
                        blob: base_blob_ref,
                        patch: Some(PatchRef {
                            archive_entry,
                            sha256,
                            algorithm_code: code,
                            algorithm_id,
                        }),
                        metadata,
                        hardlink_target: None,
                        removed: false,
                    }
                }
                EncodeOutcome::Passthrough {
                    target_bytes,
                    sha256,
                } => {
                    let size = target_bytes.len() as u64;
                    let blob_id = self.storage.upload_blob(&sha256, &target_bytes).await?;
                    self.storage
                        .record_blob_origin(
                            blob_id,
                            &options.image_id,
                            options.base_image_id.as_deref(),
                            &result.path,
                        )
                        .await?;
                    stats.total_stored_bytes += size;
                    stats.files_added += 1;
                    let metadata = if result.rename_to.is_some() || result.extra_metadata.is_some()
                    {
                        let mut m = result.extra_metadata.unwrap_or_default();
                        m.new_path = result.rename_to;
                        Some(m)
                    } else {
                        None
                    };
                    Entry {
                        path: result.path,
                        entry_type: EntryType::File,
                        size,
                        blob: Some(BlobRef { blob_id, size }),
                        patch: None,
                        metadata,
                        hardlink_target: None,
                        removed: false,
                    }
                }
            };
            entries.push(entry);
        }

        // Build + upload patches tar.
        let tar_bytes = build_patches_tar(&patches)?;
        self.storage
            .upload_patches(&options.image_id, &tar_bytes, false)
            .await?;

        // Build + upload manifest.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let manifest = Manifest {
            header: ManifestHeader {
                version: crate::manifest::MANIFEST_VERSION,
                image_id: options.image_id.clone(),
                base_image_id: options.base_image_id.clone(),
                format: "directory".into(),
                created_at: now,
                patches_compressed: false,
            },
            entries,
        };
        let manifest_bytes = rmp_serde::to_vec_named(&manifest)
            .map_err(|e| crate::Error::Manifest(e.to_string()))?;
        self.storage
            .upload_manifest(&options.image_id, &manifest_bytes)
            .await?;

        // Register image metadata.
        self.storage
            .register_image(&ImageMeta {
                image_id: options.image_id.clone(),
                base_image_id: options.base_image_id.clone(),
                format: "directory".into(),
                status: "pending".into(),
            })
            .await?;

        stats.elapsed_secs = start.elapsed().as_secs_f64();
        Ok(stats)
    }

    // ── Decompress impl ───────────────────────────────────────────────────────

    async fn decompress(
        &self,
        output_root: &Path,
        options: DecompressOptions,
    ) -> Result<DecompressionStats> {
        let start = std::time::Instant::now();
        let mut stats = DecompressionStats::default();

        // ── 1. Download + parse manifest ─────────────────────────────────────
        let manifest_bytes = self.storage.download_manifest(&options.image_id).await?;
        let manifest: Manifest = rmp_serde::from_slice(&manifest_bytes)
            .map_err(|e| crate::Error::Manifest(e.to_string()))?;

        // ── 2. Chain detection ────────────────────────────────────────────────
        if let Some(base_id) = &manifest.header.base_image_id {
            if let Some(base_meta) = self.storage.get_image(base_id).await? {
                if base_meta.base_image_id.is_some() {
                    return Err(crate::Error::Other(
                        "chained decompression is not supported: \
                         base image is itself a delta — decompress the base first"
                            .into(),
                    ));
                }
            }
        }

        // ── 3. Download + extract patches ─────────────────────────────────────
        let tar_bytes = self.storage.download_patches(&options.image_id).await?;
        let patches_map = extract_tar(&tar_bytes)?;

        // ── 4. Copy base → output ─────────────────────────────────────────────
        std::fs::create_dir_all(output_root)?;
        copy_dir_recursive(&options.base_root, output_root)?;

        // ── 5. Phase 1: apply content entries ────────────────────────────────
        let mut pending_renames: Vec<(String, String)> = Vec::new();
        let mut pending_hardlinks: Vec<(String, String)> = Vec::new();

        for entry in &manifest.entries {
            let out_path = output_root.join(&entry.path);

            // — Removed —
            if entry.removed {
                if out_path
                    .symlink_metadata()
                    .ok()
                    .is_some_and(|m| m.is_dir() && !m.file_type().is_symlink())
                {
                    let _ = std::fs::remove_dir_all(&out_path);
                } else if out_path.symlink_metadata().is_ok() {
                    let _ = std::fs::remove_file(&out_path);
                }
                stats.total_files += 1;
                continue;
            }

            // — Hardlinks deferred to Phase 2 —
            if entry.entry_type == EntryType::Hardlink {
                if let Some(hl_target) = &entry.hardlink_target {
                    pending_hardlinks.push((entry.path.clone(), hl_target.clone()));
                }
                continue;
            }

            // — Track renames for Phase 2 —
            if let Some(meta) = &entry.metadata {
                if let Some(new_path) = &meta.new_path {
                    pending_renames.push((entry.path.clone(), new_path.clone()));
                }
            }

            // — Apply content —
            match (&entry.blob, &entry.patch) {
                (Some(blob_ref), None) => {
                    let bytes = self.storage.download_blob(blob_ref.blob_id).await?;
                    self.write_content_entry(entry, &out_path, &bytes)?;
                    stats.total_bytes += bytes.len() as u64;
                    stats.total_files += 1;
                }
                (None, Some(patch_ref)) => {
                    let patch_bytes =
                        patches_map.get(&patch_ref.archive_entry).ok_or_else(|| {
                            crate::Error::Manifest(format!(
                                "patch archive entry not found: {}",
                                patch_ref.archive_entry
                            ))
                        })?;
                    let actual_sha = sha256_hex(patch_bytes);
                    if actual_sha != patch_ref.sha256 {
                        return Err(crate::Error::Manifest(format!(
                            "patch SHA-256 mismatch for '{}': expected {}, got {}",
                            entry.path, patch_ref.sha256, actual_sha
                        )));
                    }
                    let source_bytes = std::fs::read(&out_path)?;
                    let file_patch = crate::FilePatch {
                        bytes: patch_bytes.clone(),
                        code: patch_ref.algorithm_code,
                        algorithm_id: patch_ref.algorithm_id.clone(),
                    };
                    let result = self.router.decode(&source_bytes, &file_patch)?;
                    stats.total_bytes += result.len() as u64;
                    std::fs::write(&out_path, &result)?;
                    stats.total_files += 1;
                    stats.patches_verified += 1;
                }
                (Some(blob_ref), Some(patch_ref)) => {
                    // BlobPatch: download blob, apply patch on top.
                    let blob_bytes = self.storage.download_blob(blob_ref.blob_id).await?;
                    let patch_bytes =
                        patches_map.get(&patch_ref.archive_entry).ok_or_else(|| {
                            crate::Error::Manifest(format!(
                                "BlobPatch archive entry not found: {}",
                                patch_ref.archive_entry
                            ))
                        })?;
                    let actual_sha = sha256_hex(patch_bytes);
                    if actual_sha != patch_ref.sha256 {
                        return Err(crate::Error::Manifest(format!(
                            "BlobPatch SHA-256 mismatch for '{}': expected {}, got {}",
                            entry.path, patch_ref.sha256, actual_sha
                        )));
                    }
                    let file_patch = crate::FilePatch {
                        bytes: patch_bytes.clone(),
                        code: patch_ref.algorithm_code,
                        algorithm_id: patch_ref.algorithm_id.clone(),
                    };
                    let result = self.router.decode(&blob_bytes, &file_patch)?;
                    create_parent_dirs(&out_path)?;
                    std::fs::write(&out_path, &result)?;
                    stats.total_bytes += result.len() as u64;
                    stats.total_files += 1;
                    stats.patches_verified += 1;
                }
                (None, None) => {
                    // Metadata-only or rename-only.
                    if entry.entry_type == EntryType::Symlink {
                        if let Some(meta) = &entry.metadata {
                            if let Some(link_target) = &meta.link_target {
                                if out_path.symlink_metadata().is_ok() {
                                    std::fs::remove_file(&out_path)?;
                                }
                                std::os::unix::fs::symlink(link_target, &out_path)?;
                            }
                        }
                    } else if entry.entry_type == EntryType::Directory {
                        std::fs::create_dir_all(&out_path)?;
                    }
                    stats.total_files += 1;
                }
            }

            // Apply mode/mtime metadata (new_path is Phase 2).
            if let Some(meta) = &entry.metadata {
                apply_metadata(output_root, &entry.path, meta)?;
            }
        }

        // ── 6. Phase 2: renames ───────────────────────────────────────────────
        for (old_path, new_path) in pending_renames {
            let old = output_root.join(&old_path);
            let new = output_root.join(&new_path);
            create_parent_dirs(&new)?;
            std::fs::rename(&old, &new)?;
        }

        // ── 7. Phase 2: hardlinks ─────────────────────────────────────────────
        for (path, hl_target) in pending_hardlinks {
            let target_abs = output_root.join(&hl_target);
            let link_abs = output_root.join(&path);
            create_parent_dirs(&link_abs)?;
            std::fs::hard_link(&target_abs, &link_abs)?;
            stats.total_files += 1;
        }

        stats.elapsed_secs = start.elapsed().as_secs_f64();
        Ok(stats)
    }
}

// ── DefaultCompressor entry-building helpers ──────────────────────────────────

impl DefaultCompressor {
    /// Classify an added file: push a non-encoding entry into `immediate_entries`
    /// (hardlinks, symlinks, directories) or defer blob upload to Phase 3.
    fn classify_added_entry(
        &self,
        target_file: &Path,
        path: &str,
        inode_map: &HashMap<(u64, u64), String>,
        immediate_entries: &mut Vec<Entry>,
        deferred_blobs: &mut Vec<DeferredBlob>,
        stats: &mut CompressionStats,
    ) -> Result<()> {
        let meta = target_file.symlink_metadata()?;

        // Hardlink detection: regular file with nlink > 1 sharing an inode
        // with another path that was already seen in the walk.
        if meta.file_type().is_file() && meta.nlink() > 1 {
            let key = (meta.dev(), meta.ino());
            if let Some(canonical) = inode_map.get(&key) {
                if canonical != path {
                    immediate_entries.push(Entry {
                        path: path.to_string(),
                        entry_type: EntryType::Hardlink,
                        size: 0,
                        blob: None,
                        patch: None,
                        metadata: None,
                        hardlink_target: Some(canonical.clone()),
                        removed: false,
                    });
                    return Ok(());
                }
            }
        }

        if meta.file_type().is_symlink() {
            let link_target = std::fs::read_link(target_file)?
                .to_string_lossy()
                .into_owned();
            immediate_entries.push(Entry {
                path: path.to_string(),
                entry_type: EntryType::Symlink,
                size: 0,
                blob: None,
                patch: None,
                metadata: Some(Metadata {
                    link_target: Some(link_target),
                    ..Default::default()
                }),
                hardlink_target: None,
                removed: false,
            });
            return Ok(());
        }

        if meta.file_type().is_dir() {
            immediate_entries.push(Entry {
                path: path.to_string(),
                entry_type: EntryType::Directory,
                size: 0,
                blob: None,
                patch: None,
                metadata: None,
                hardlink_target: None,
                removed: false,
            });
            return Ok(());
        }

        // Regular file → defer blob upload to Phase 3.
        let bytes = std::fs::read(target_file)?;
        let size = bytes.len() as u64;
        let sha = sha256_hex(&bytes);
        stats.files_added += 1;
        stats.total_source_bytes += size;
        deferred_blobs.push(DeferredBlob {
            path: path.to_string(),
            bytes,
            sha256: sha,
            size,
        });
        Ok(())
    }

    /// Classify a renamed file: push a non-encoding entry into `immediate_entries`
    /// (pure renames, symlink renames) or create an `EncodeTask` for content changes.
    #[allow(clippy::too_many_arguments)]
    fn classify_rename_entry(
        &self,
        source_file: &Path,
        target_file: &Path,
        source_path: &str,
        target_path: &str,
        passthrough_threshold: f64,
        immediate_entries: &mut Vec<Entry>,
        encode_tasks: &mut Vec<EncodeTask>,
        stats: &mut CompressionStats,
    ) -> Result<()> {
        let s_meta = source_file.symlink_metadata()?;

        if s_meta.file_type().is_symlink() {
            let new_link_target = std::fs::read_link(target_file)?
                .to_string_lossy()
                .into_owned();
            immediate_entries.push(Entry {
                path: source_path.to_string(),
                entry_type: EntryType::Symlink,
                size: 0,
                blob: None,
                patch: None,
                metadata: Some(Metadata {
                    new_path: Some(target_path.to_string()),
                    link_target: Some(new_link_target),
                    ..Default::default()
                }),
                hardlink_target: None,
                removed: false,
            });
            return Ok(());
        }

        let source_bytes = std::fs::read(source_file)?;
        let target_bytes = std::fs::read(target_file)?;
        let size = target_bytes.len() as u64;
        stats.total_source_bytes += size;

        if sha256_hex(&source_bytes) == sha256_hex(&target_bytes) {
            // Pure rename — content unchanged, no encoding needed.
            immediate_entries.push(Entry {
                path: source_path.to_string(),
                entry_type: EntryType::File,
                size,
                blob: None,
                patch: None,
                metadata: Some(Metadata {
                    new_path: Some(target_path.to_string()),
                    ..Default::default()
                }),
                hardlink_target: None,
                removed: false,
            });
            return Ok(());
        }

        // Rename + content change → encoding task.
        encode_tasks.push(EncodeTask {
            path: source_path.to_string(),
            rename_to: Some(target_path.to_string()),
            source_bytes,
            target_bytes,
            target_size: size,
            passthrough_threshold,
            extra_metadata: None,
            base_blob_ref: None,
        });
        Ok(())
    }

    /// Classify a changed file: push a non-encoding entry into `immediate_entries`
    /// (symlink changes) or create an `EncodeTask` for regular file content changes.
    #[allow(clippy::too_many_arguments)]
    fn classify_changed_entry(
        &self,
        source_file: &Path,
        target_file: &Path,
        path: &str,
        passthrough_threshold: f64,
        immediate_entries: &mut Vec<Entry>,
        encode_tasks: &mut Vec<EncodeTask>,
        stats: &mut CompressionStats,
    ) -> Result<()> {
        let t_meta = target_file.symlink_metadata()?;

        if t_meta.file_type().is_symlink() {
            let link_target = std::fs::read_link(target_file)?
                .to_string_lossy()
                .into_owned();
            let extra = collect_metadata_changes(source_file, target_file).unwrap_or_default();
            immediate_entries.push(Entry {
                path: path.to_string(),
                entry_type: EntryType::Symlink,
                size: 0,
                blob: None,
                patch: None,
                metadata: Some(Metadata {
                    link_target: Some(link_target),
                    mode: extra.mode,
                    uid: extra.uid,
                    gid: extra.gid,
                    ..Default::default()
                }),
                hardlink_target: None,
                removed: false,
            });
            return Ok(());
        }

        let source_bytes = std::fs::read(source_file)?;
        let target_bytes = std::fs::read(target_file)?;
        let size = target_bytes.len() as u64;
        stats.total_source_bytes += size;

        encode_tasks.push(EncodeTask {
            path: path.to_string(),
            rename_to: None,
            source_bytes,
            target_bytes,
            target_size: size,
            passthrough_threshold,
            extra_metadata: None,
            base_blob_ref: None,
        });
        Ok(())
    }

    /// Write a blob-backed entry to `out_path` (handles symlinks vs regular files).
    fn write_content_entry(&self, entry: &Entry, out_path: &Path, bytes: &[u8]) -> Result<()> {
        if entry.entry_type == EntryType::Symlink {
            let link_target =
                std::str::from_utf8(bytes).map_err(|e| crate::Error::Other(e.to_string()))?;
            if out_path.symlink_metadata().is_ok() {
                std::fs::remove_file(out_path)?;
            }
            std::os::unix::fs::symlink(link_target, out_path)?;
        } else {
            create_parent_dirs(out_path)?;
            std::fs::write(out_path, bytes)?;
        }
        Ok(())
    }
}
