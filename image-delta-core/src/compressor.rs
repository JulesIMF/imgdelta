use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::manifest::{BlobRef, Entry, EntryType, Manifest, ManifestHeader, Metadata, PatchRef};
use crate::path_match::{find_best_matches, PathMatchConfig};
use crate::storage::ImageMeta;
use crate::{DeltaEncoder, Result, Storage};

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
pub trait Compressor: Send + Sync {
    /// Compress `target_root` relative to `source_root` and store the manifest
    /// and patches via the [`Storage`] backend.
    fn compress(
        &self,
        source_root: &Path,
        target_root: &Path,
        options: CompressOptions,
    ) -> Result<CompressionStats>;

    /// Download patches from storage and reconstruct the image at `output_root`.
    fn decompress(
        &self,
        output_root: &Path,
        options: DecompressOptions,
    ) -> Result<DecompressionStats>;
}

// ── DefaultCompressor ─────────────────────────────────────────────────────────

/// Production [`Compressor`] implementation.
///
/// Owns a [`Storage`] backend and a [`DeltaEncoder`].  The encoder may be a
/// [`RouterEncoder`] for per-file encoder selection.
///
/// [`RouterEncoder`]: crate::RouterEncoder
pub struct DefaultCompressor {
    storage: Arc<dyn Storage>,
    encoder: Arc<dyn DeltaEncoder>,
}

impl DefaultCompressor {
    /// Create a new `DefaultCompressor` backed by the given storage and encoder.
    ///
    /// Pass a [`RouterEncoder`] as `encoder` to enable per-file algorithm selection.
    ///
    /// [`RouterEncoder`]: crate::RouterEncoder
    pub fn new(storage: Arc<dyn Storage>, encoder: Arc<dyn DeltaEncoder>) -> Self {
        Self { storage, encoder }
    }
}

// ── Module-level helpers ──────────────────────────────────────────────────────

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

impl Compressor for DefaultCompressor {
    fn compress(
        &self,
        source_root: &Path,
        target_root: &Path,
        options: CompressOptions,
    ) -> Result<CompressionStats> {
        use crate::fs_diff::{diff_dirs, DiffKind};
        use std::time::{SystemTime, UNIX_EPOCH};

        let mut stats = CompressionStats::default();
        let start = std::time::Instant::now();

        // ── 1. Diff ──────────────────────────────────────────────────────────
        let diff = diff_dirs(source_root, target_root)?;

        // ── 2. Path-match for rename detection ───────────────────────────────
        let removed_paths: Vec<String> = diff.removed().map(|d| d.path.clone()).collect();
        let added_paths: Vec<String> = diff.added().map(|d| d.path.clone()).collect();
        let renames = find_best_matches(&removed_paths, &added_paths, &PathMatchConfig::default())?;

        let renamed_sources: HashSet<String> =
            renames.iter().map(|r| r.source_path.clone()).collect();
        let renamed_targets: HashMap<String, String> = renames
            .iter()
            .map(|r| (r.target_path.clone(), r.source_path.clone()))
            .collect();

        // ── 3. Inode map for hardlink detection ──────────────────────────────
        let inode_map = build_inode_map(target_root)?;

        // ── 4. Build manifest entries ────────────────────────────────────────
        let mut entries: Vec<Entry> = Vec::new();
        let mut patches: Vec<(String, Vec<u8>)> = Vec::new();

        // — Removed (not renamed) —
        for d in diff.removed() {
            if renamed_sources.contains(&d.path) {
                continue;
            }
            entries.push(Entry {
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

        // — Added (not target of a rename) —
        for d in diff.added() {
            if renamed_targets.contains_key(&d.path) {
                continue;
            }
            let target_file = target_root.join(&d.path);
            let entry = self.compress_added_entry(&target_file, &d.path, &inode_map, &mut stats)?;
            entries.push(entry);
        }

        // — Renames —
        for rename in &renames {
            let source_file = source_root.join(&rename.source_path);
            let target_file = target_root.join(&rename.target_path);
            let entry = self.compress_rename_entry(
                &source_file,
                &target_file,
                &rename.source_path,
                &rename.target_path,
                &mut patches,
                &mut stats,
            )?;
            entries.push(entry);
        }

        // — Changed —
        for d in diff.diffs.iter().filter(|d| d.kind == DiffKind::Changed) {
            let source_file = source_root.join(&d.path);
            let target_file = target_root.join(&d.path);
            let entry = self.compress_changed_entry(
                &source_file,
                &target_file,
                &d.path,
                &mut patches,
                &mut stats,
                options.passthrough_threshold,
            )?;
            entries.push(entry);
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
            entries.push(Entry {
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

        // ── 5. Build + upload patches tar ────────────────────────────────────
        let tar_bytes = build_patches_tar(&patches)?;
        self.storage.upload_patches(&options.image_id, &tar_bytes)?;

        // ── 6. Build + upload manifest ───────────────────────────────────────
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
            .upload_manifest(&options.image_id, &manifest_bytes)?;

        // ── 7. Save image meta ────────────────────────────────────────────────
        self.storage.save_image_meta(&ImageMeta {
            image_id: options.image_id.clone(),
            base_image_id: options.base_image_id.clone(),
            format: "directory".into(),
        })?;

        stats.elapsed_secs = start.elapsed().as_secs_f64();
        Ok(stats)
    }

    // ── Decompress impl ───────────────────────────────────────────────────────

    fn decompress(
        &self,
        output_root: &Path,
        options: DecompressOptions,
    ) -> Result<DecompressionStats> {
        let start = std::time::Instant::now();
        let mut stats = DecompressionStats::default();

        // ── 1. Download + parse manifest ─────────────────────────────────────
        let manifest_bytes = self.storage.download_manifest(&options.image_id)?;
        let manifest: Manifest = rmp_serde::from_slice(&manifest_bytes)
            .map_err(|e| crate::Error::Manifest(e.to_string()))?;

        // ── 2. Chain detection ────────────────────────────────────────────────
        if let Some(base_id) = &manifest.header.base_image_id {
            if let Some(base_meta) = self.storage.get_image_meta(base_id)? {
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
        let tar_bytes = self.storage.download_patches(&options.image_id)?;
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
                    let bytes = self.storage.download_blob(blob_ref.blob_id)?;
                    self.write_content_entry(entry, &out_path, &bytes)?;
                    stats.total_bytes += bytes.len() as u64;
                    stats.total_files += 1;
                }
                (None, Some(patch_ref)) => {
                    if patch_ref.algorithm != self.encoder.algorithm_id() {
                        return Err(crate::Error::Other(format!(
                            "unsupported patch algorithm '{}' (encoder is '{}')",
                            patch_ref.algorithm,
                            self.encoder.algorithm_id()
                        )));
                    }
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
                    let result = self.encoder.decode(&source_bytes, patch_bytes)?;
                    stats.total_bytes += result.len() as u64;
                    std::fs::write(&out_path, &result)?;
                    stats.total_files += 1;
                    stats.patches_verified += 1;
                }
                (Some(blob_ref), Some(patch_ref)) => {
                    // BlobPatch: download blob, apply patch on top
                    if patch_ref.algorithm != self.encoder.algorithm_id() {
                        return Err(crate::Error::Other(format!(
                            "unsupported patch algorithm '{}' (encoder is '{}')",
                            patch_ref.algorithm,
                            self.encoder.algorithm_id()
                        )));
                    }
                    let blob_bytes = self.storage.download_blob(blob_ref.blob_id)?;
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
                    let result = self.encoder.decode(&blob_bytes, patch_bytes)?;
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
    fn compress_added_entry(
        &self,
        target_file: &Path,
        path: &str,
        inode_map: &HashMap<(u64, u64), String>,
        stats: &mut CompressionStats,
    ) -> Result<Entry> {
        let meta = target_file.symlink_metadata()?;

        // Hardlink detection: regular file with nlink > 1 sharing an inode
        // with another path that was seen first in the walk.
        if meta.file_type().is_file() && meta.nlink() > 1 {
            let key = (meta.dev(), meta.ino());
            if let Some(canonical) = inode_map.get(&key) {
                if canonical != path {
                    return Ok(Entry {
                        path: path.to_string(),
                        entry_type: EntryType::Hardlink,
                        size: 0,
                        blob: None,
                        patch: None,
                        metadata: None,
                        hardlink_target: Some(canonical.clone()),
                        removed: false,
                    });
                }
            }
        }

        if meta.file_type().is_symlink() {
            let link_target = std::fs::read_link(target_file)?
                .to_string_lossy()
                .into_owned();
            return Ok(Entry {
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
        }

        if meta.file_type().is_dir() {
            return Ok(Entry {
                path: path.to_string(),
                entry_type: EntryType::Directory,
                size: 0,
                blob: None,
                patch: None,
                metadata: None,
                hardlink_target: None,
                removed: false,
            });
        }

        // Regular file → upload as blob.
        let bytes = std::fs::read(target_file)?;
        let size = bytes.len() as u64;
        let blob_id = self.storage.upload_blob(&bytes)?;
        stats.files_added += 1;
        stats.total_source_bytes += size;
        stats.total_stored_bytes += size;
        Ok(Entry {
            path: path.to_string(),
            entry_type: EntryType::File,
            size,
            blob: Some(BlobRef { blob_id, size }),
            patch: None,
            metadata: None,
            hardlink_target: None,
            removed: false,
        })
    }

    fn compress_rename_entry(
        &self,
        source_file: &Path,
        target_file: &Path,
        source_path: &str,
        target_path: &str,
        patches: &mut Vec<(String, Vec<u8>)>,
        stats: &mut CompressionStats,
    ) -> Result<Entry> {
        let s_meta = source_file.symlink_metadata()?;

        if s_meta.file_type().is_symlink() {
            let new_link_target = std::fs::read_link(target_file)?
                .to_string_lossy()
                .into_owned();
            return Ok(Entry {
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
        }

        let source_bytes = std::fs::read(source_file)?;
        let target_bytes = std::fs::read(target_file)?;
        let size = target_bytes.len() as u64;
        stats.total_source_bytes += size;

        if sha256_hex(&source_bytes) == sha256_hex(&target_bytes) {
            // Pure rename, content unchanged.
            return Ok(Entry {
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
        }

        // Rename + content change → delta.
        let delta = self.encoder.encode(&source_bytes, &target_bytes)?;
        let archive_entry = format!("{}.patch", uuid::Uuid::new_v4());
        let sha = sha256_hex(&delta);
        stats.total_stored_bytes += delta.len() as u64;
        patches.push((archive_entry.clone(), delta));
        stats.files_patched += 1;
        Ok(Entry {
            path: source_path.to_string(),
            entry_type: EntryType::File,
            size,
            blob: None,
            patch: Some(PatchRef {
                archive_entry,
                sha256: sha,
                algorithm: self.encoder.algorithm_id().to_string(),
            }),
            metadata: Some(Metadata {
                new_path: Some(target_path.to_string()),
                ..Default::default()
            }),
            hardlink_target: None,
            removed: false,
        })
    }

    fn compress_changed_entry(
        &self,
        source_file: &Path,
        target_file: &Path,
        path: &str,
        patches: &mut Vec<(String, Vec<u8>)>,
        stats: &mut CompressionStats,
        passthrough_threshold: f64,
    ) -> Result<Entry> {
        let t_meta = target_file.symlink_metadata()?;

        if t_meta.file_type().is_symlink() {
            let link_target = std::fs::read_link(target_file)?
                .to_string_lossy()
                .into_owned();
            let extra = collect_metadata_changes(source_file, target_file).unwrap_or_default();
            return Ok(Entry {
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
        }

        let source_bytes = std::fs::read(source_file)?;
        let target_bytes = std::fs::read(target_file)?;
        let size = target_bytes.len() as u64;
        stats.total_source_bytes += size;

        let delta = self.encoder.encode(&source_bytes, &target_bytes)?;
        let use_delta = (delta.len() as f64) < (target_bytes.len() as f64) * passthrough_threshold;

        if use_delta {
            let archive_entry = format!("{}.patch", uuid::Uuid::new_v4());
            let sha = sha256_hex(&delta);
            stats.total_stored_bytes += delta.len() as u64;
            patches.push((archive_entry.clone(), delta));
            stats.files_patched += 1;
            Ok(Entry {
                path: path.to_string(),
                entry_type: EntryType::File,
                size,
                blob: None,
                patch: Some(PatchRef {
                    archive_entry,
                    sha256: sha,
                    algorithm: self.encoder.algorithm_id().to_string(),
                }),
                metadata: None,
                hardlink_target: None,
                removed: false,
            })
        } else {
            let blob_id = self.storage.upload_blob(&target_bytes)?;
            stats.total_stored_bytes += size;
            stats.files_added += 1;
            Ok(Entry {
                path: path.to_string(),
                entry_type: EntryType::File,
                size,
                blob: Some(BlobRef { blob_id, size }),
                patch: None,
                metadata: None,
                hardlink_target: None,
                removed: false,
            })
        }
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
