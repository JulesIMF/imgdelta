// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Eight-stage FS-partition compression pipeline (walkdir → patch → archive)

//! Eight-stage stateless compress pipeline for one `Fs` partition.
//!
//! Each stage is a standalone function that takes a [`FsDraft`] (plus optional
//! helpers) and returns a transformed `FsDraft`.  Stages are independently
//! testable — no stage depends on the output of a stage it does not directly
//! receive.
//!
//! ## Pipeline overview
//!
//! | # | Function                          | Pure? | Notes                               |
//! |---|-----------------------------------|-------|-------------------------------------|
//! | 1 | [`walkdir`]                       | yes   | Walk base + target, build records   |
//! | 2 | [`s3_lookup`]                     | async | Match added files against S3 blobs  |
//! | 3 | [`match_renamed`]                 | yes   | Detect renames via path matching    |
//! | 4 | [`cleanup`]                       | yes   | Finalise deletion records           |
//! | 5 | [`upload_lazy_blobs`]             | async | Upload new file content to S3       |
//! | 6 | [`download_blobs_for_patches`]    | async | Download delta-base blobs to disk   |
//! | 7 | [`compute_patches`]               | yes   | Encode all patches (rayon)          |
//! | 8 | [`pack_and_upload_archive`]       | async | Pack + upload patches tar archive   |
//!
//! The orchestrator [`compress_fs_partition`] chains all stages and returns a
//! [`PartitionManifest`].

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use sha2::{Digest, Sha256};
use tracing::{debug, info};
use walkdir::WalkDir;

use crate::algorithm::FileSnapshot;
use crate::manifest::{
    BlobRef, Data, DataRef, DeviceInfo, EntryType, Metadata, PartitionContent, PartitionManifest,
    Patch, PatchRef, Record,
};
use crate::partition::PartitionDescriptor;
use crate::path_match::{find_best_matches, PathMatchConfig};
use crate::routing::{FileInfo, RouterEncoder};
use crate::storage::Storage;
use crate::{PassthroughEncoder, PatchEncoder, Result};

#[cfg(unix)]
use libc;

// ── FsDraft ───────────────────────────────────────────────────────────────────

/// Mutable working state passed through the compress pipeline stages.
///
/// After [`pack_and_upload_archive`] the draft is consumed and a
/// [`PartitionContent::Fs`] is returned.
#[derive(Debug, Default)]
pub struct FsDraft {
    /// File-level change records for this partition.
    ///
    /// Progressively refined across stages:
    /// - Stage 1: raw `Patch::Lazy` / `Data::LazyBlob` / `Data::OriginalFile`
    /// - Stage 5: `LazyBlob` → `BlobRef`
    /// - Stage 6: `DataRef::BlobRef` in patches → `DataRef::FilePath`
    /// - Stage 7: `Patch::Lazy` → `Patch::Real`
    pub records: Vec<Record>,

    /// Temporary files downloaded from S3 for patch computation.
    ///
    /// All paths in this list must be removed by the caller after
    /// [`compute_patches`] completes.
    pub tmp_files: Vec<PathBuf>,

    /// Raw patch bytes indexed by archive-entry name.
    ///
    /// Populated by [`compute_patches`], consumed (and cleared) by
    /// [`pack_and_upload_archive`].
    pub patch_bytes: HashMap<String, Vec<u8>>,
}

// ── Internal filesystem snapshot ──────────────────────────────────────────────

#[derive(Debug)]
struct EntrySnapshot {
    is_file: bool,
    is_symlink: bool,
    is_dir: bool,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime_secs: i64,
    /// SHA-256 of content — `Some` only for regular files.
    sha256: Option<[u8; 32]>,
    /// Symlink target string — `Some` only for symlinks.
    link_target: Option<String>,
    /// Byte size — meaningful only for regular files.
    size: u64,
    dev_ino: (u64, u64),
    nlink: u64,
    /// Raw device number (`st_rdev`) — set only for special files.
    rdev: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 1 — walkdir
// ─────────────────────────────────────────────────────────────────────────────

/// Walk `base_root` and `target_root` and produce the initial [`FsDraft`].
///
/// Only *changed* entries appear in `draft.records` — unchanged ones are
/// silently skipped.  Hard-link groups in the target tree are detected by
/// `(dev, ino)` and recorded with `Data::HardlinkTo(canonical)` for
/// non-canonical members.
///
/// # Entry semantics
///
/// | `old_path` | `new_path` | Meaning                    |
/// |------------|------------|----------------------------|
/// | `None`     | `Some`     | Added in target             |
/// | `Some`     | `None`     | Removed from base           |
/// | `Some`     | `Some`     | Changed in-place            |
pub fn walkdir(base_root: &Path, target_root: &Path) -> Result<FsDraft> {
    debug!(base_root = %base_root.display(), target_root = %target_root.display(), "walkdir: scanning paths");
    let base_snap = snapshot(base_root)?;
    let target_snap = snapshot(target_root)?;
    debug!(
        base_entries = base_snap.len(),
        target_entries = target_snap.len(),
        "walkdir: snapshot sizes"
    );

    // Detect hardlink groups in target: (dev, ino) → canonical path (first alphabetically).
    let hardlink_canonicals: HashMap<(u64, u64), String> = {
        let mut groups: HashMap<(u64, u64), Vec<&str>> = HashMap::new();
        for (path, e) in &target_snap {
            if e.is_file && e.nlink > 1 {
                groups.entry(e.dev_ino).or_default().push(path.as_str());
            }
        }
        groups
            .into_iter()
            .filter_map(|(dev_ino, mut paths)| {
                if paths.len() > 1 {
                    paths.sort_unstable();
                    Some((dev_ino, paths[0].to_string()))
                } else {
                    None
                }
            })
            .collect()
    };

    let mut records: Vec<Record> = Vec::new();

    // Removed: present in base, absent in target.
    for (path, b) in &base_snap {
        if !target_snap.contains_key(path.as_str()) {
            debug!(path, "removed");
            records.push(make_removed_record(path, b, base_root));
        }
    }

    // Added: present in target, absent in base.
    for (path, t) in &target_snap {
        if !base_snap.contains_key(path.as_str()) {
            debug!(path, "added");
            records.push(make_added_record(
                path,
                t,
                target_root,
                &hardlink_canonicals,
            ));
        }
    }

    // Changed: present in both.
    for (path, t) in &target_snap {
        if let Some(b) = base_snap.get(path.as_str()) {
            let new_recs = diff_entry(path, b, t, base_root, target_root, &hardlink_canonicals);
            if !new_recs.is_empty() {
                debug!(path, "changed");
            }
            records.extend(new_recs);
        }
    }

    Ok(FsDraft {
        records,
        ..Default::default()
    })
}

// ── Snapshot helpers ──────────────────────────────────────────────────────────

fn snapshot(root: &Path) -> Result<HashMap<String, EntrySnapshot>> {
    let mut map = HashMap::new();
    for entry_result in WalkDir::new(root).follow_links(false) {
        let entry = entry_result.map_err(|e| std::io::Error::other(format!("walkdir: {e}")))?;
        if entry.path() == root {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        let meta = entry.path().symlink_metadata()?;
        let ft = meta.file_type();
        let is_symlink = ft.is_symlink();
        let is_file = ft.is_file();
        let is_dir = ft.is_dir();

        let sha256 = if is_file {
            Some(sha256_of_file(entry.path())?)
        } else {
            None
        };
        let link_target = if is_symlink {
            Some(
                std::fs::read_link(entry.path())?
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            None
        };
        let rdev = if !is_file && !is_dir && !is_symlink {
            meta.rdev()
        } else {
            0
        };

        map.insert(
            rel,
            EntrySnapshot {
                is_file,
                is_symlink,
                is_dir,
                mode: meta.mode(),
                uid: meta.uid(),
                gid: meta.gid(),
                mtime_secs: meta.mtime(),
                sha256,
                link_target,
                size: if is_file { meta.size() } else { 0 },
                dev_ino: (meta.dev(), meta.ino()),
                nlink: meta.nlink(),
                rdev,
            },
        );
    }
    Ok(map)
}

pub(crate) fn sha256_of_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

// ── Record constructors ───────────────────────────────────────────────────────

fn snap_entry_type(e: &EntrySnapshot) -> EntryType {
    if e.is_file {
        EntryType::File
    } else if e.is_symlink {
        EntryType::Symlink
    } else if e.is_dir {
        EntryType::Directory
    } else {
        EntryType::Special
    }
}

/// Extract the device_info for a special-file snapshot.
#[cfg(unix)]
fn device_info_from_snapshot(e: &EntrySnapshot) -> DeviceInfo {
    DeviceInfo {
        file_type_bits: e.mode & 0o170000,
        major: libc::major(e.rdev) as u32,
        minor: libc::minor(e.rdev) as u32,
    }
}

fn metadata_from_snapshot(e: &EntrySnapshot) -> Metadata {
    Metadata {
        mode: Some(e.mode),
        uid: Some(e.uid),
        gid: Some(e.gid),
        mtime: Some(e.mtime_secs),
        xattrs: None,
    }
}

/// Compute the metadata fields that *differ* between base and target.
///
/// Returns `None` when all tracked fields are equal (within ±1 s mtime
/// tolerance).
fn metadata_diff(b: &EntrySnapshot, t: &EntrySnapshot) -> Option<Metadata> {
    let mode = (b.mode != t.mode).then_some(t.mode);
    let uid = (b.uid != t.uid).then_some(t.uid);
    let gid = (b.gid != t.gid).then_some(t.gid);
    let mtime = (b.mtime_secs.abs_diff(t.mtime_secs) > 1).then_some(t.mtime_secs);
    if mode.is_none() && uid.is_none() && gid.is_none() && mtime.is_none() {
        None
    } else {
        Some(Metadata {
            mode,
            uid,
            gid,
            mtime,
            xattrs: None,
        })
    }
}

fn make_removed_record(path: &str, b: &EntrySnapshot, base_root: &Path) -> Record {
    let data = if b.is_file {
        // Keep path so rename-matching (stage 3) can use it as a delta base.
        Some(Data::OriginalFile(base_root.join(path)))
    } else {
        None
    };
    Record {
        old_path: Some(path.to_string()),
        new_path: None,
        entry_type: snap_entry_type(b),
        size: b.size,
        data,
        patch: None,
        metadata: None,
    }
}

fn make_added_record(
    path: &str,
    t: &EntrySnapshot,
    target_root: &Path,
    hardlink_canonicals: &HashMap<(u64, u64), String>,
) -> Record {
    let metadata = Some(metadata_from_snapshot(t));

    if t.is_symlink {
        return Record {
            old_path: None,
            new_path: Some(path.to_string()),
            entry_type: EntryType::Symlink,
            size: 0,
            data: Some(Data::SoftlinkTo(t.link_target.clone().unwrap_or_default())),
            patch: None,
            metadata,
        };
    }

    if t.is_dir {
        return Record {
            old_path: None,
            new_path: Some(path.to_string()),
            entry_type: EntryType::Directory,
            size: 0,
            data: None,
            patch: None,
            metadata,
        };
    }

    // Regular file — check for non-canonical hardlink.
    if t.nlink > 1 {
        if let Some(canonical) = hardlink_canonicals.get(&t.dev_ino) {
            if canonical.as_str() != path {
                return Record {
                    old_path: None,
                    new_path: Some(path.to_string()),
                    entry_type: EntryType::Hardlink,
                    size: 0,
                    data: Some(Data::HardlinkTo(canonical.clone())),
                    patch: None,
                    metadata,
                };
            }
        }
    }

    // Special file (char/block device, FIFO, socket).
    if !t.is_file && !t.is_dir && !t.is_symlink {
        #[cfg(unix)]
        let dev_data = Some(Data::SpecialDevice(device_info_from_snapshot(t)));
        #[cfg(not(unix))]
        let dev_data: Option<Data> = None;
        return Record {
            old_path: None,
            new_path: Some(path.to_string()),
            entry_type: EntryType::Special,
            size: 0,
            data: dev_data,
            patch: None,
            metadata,
        };
    }

    Record {
        old_path: None,
        new_path: Some(path.to_string()),
        entry_type: EntryType::File,
        size: t.size,
        data: Some(Data::LazyBlob(target_root.join(path))),
        patch: None,
        metadata,
    }
}

/// Emit changed records for a path present in *both* trees.
///
/// Returns 0 records when nothing changed, 1 for in-place changes, 2 for type
/// conflicts (old type deleted + new type added).
fn diff_entry(
    path: &str,
    b: &EntrySnapshot,
    t: &EntrySnapshot,
    base_root: &Path,
    target_root: &Path,
    hardlink_canonicals: &HashMap<(u64, u64), String>,
) -> Vec<Record> {
    // Type conflict → delete old + add new.
    if b.is_file != t.is_file || b.is_symlink != t.is_symlink || b.is_dir != t.is_dir {
        return vec![
            make_removed_record(path, b, base_root),
            make_added_record(path, t, target_root, hardlink_canonicals),
        ];
    }

    if t.is_dir {
        return metadata_diff(b, t)
            .map(|meta| {
                vec![Record {
                    old_path: Some(path.to_string()),
                    new_path: Some(path.to_string()),
                    entry_type: EntryType::Directory,
                    size: 0,
                    data: None,
                    patch: None,
                    metadata: Some(meta),
                }]
            })
            .unwrap_or_default();
    }

    if t.is_symlink {
        let target_changed = b.link_target != t.link_target;
        let meta = metadata_diff(b, t);
        if !target_changed && meta.is_none() {
            return vec![];
        }
        let patch = target_changed.then(|| Patch::Lazy {
            old_data: DataRef::FilePath(base_root.join(path)),
            new_data: DataRef::FilePath(target_root.join(path)),
        });
        return vec![Record {
            old_path: Some(path.to_string()),
            new_path: Some(path.to_string()),
            entry_type: EntryType::Symlink,
            size: 0,
            data: None,
            patch,
            metadata: meta,
        }];
    }

    // Special file (char/block device, FIFO, socket).
    if !t.is_file && !t.is_dir && !t.is_symlink {
        // If raw device number or file-type bits changed → remove + add.
        let type_or_dev_changed = (b.mode & 0o170000) != (t.mode & 0o170000) || b.rdev != t.rdev;
        if type_or_dev_changed {
            return vec![
                make_removed_record(path, b, base_root),
                make_added_record(path, t, target_root, hardlink_canonicals),
            ];
        }
        // Only metadata (uid/gid/mtime/mode-bits) changed.
        return metadata_diff(b, t)
            .map(|meta| {
                #[cfg(unix)]
                let dev_data = Some(Data::SpecialDevice(device_info_from_snapshot(t)));
                #[cfg(not(unix))]
                let dev_data: Option<Data> = None;
                vec![Record {
                    old_path: Some(path.to_string()),
                    new_path: Some(path.to_string()),
                    entry_type: EntryType::Special,
                    size: 0,
                    data: dev_data,
                    patch: None,
                    metadata: Some(meta),
                }]
            })
            .unwrap_or_default();
    }

    // Regular file.
    debug_assert!(t.is_file);
    let content_changed = match (b.sha256, t.sha256) {
        (Some(bh), Some(th)) => bh != th,
        _ => true,
    };
    let meta = metadata_diff(b, t);

    if !content_changed && meta.is_none() {
        return vec![];
    }

    let patch = content_changed.then(|| Patch::Lazy {
        old_data: DataRef::FilePath(base_root.join(path)),
        new_data: DataRef::FilePath(target_root.join(path)),
    });

    vec![Record {
        old_path: Some(path.to_string()),
        new_path: Some(path.to_string()),
        entry_type: EntryType::File,
        size: t.size,
        data: None,
        patch,
        metadata: meta,
    }]
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 2 — s3_lookup
// ─────────────────────────────────────────────────────────────────────────────

/// For each newly added regular file (`old_path = None, data = LazyBlob`),
/// find a matching blob in `base_image_id` via path similarity.
///
/// Files that find a match are upgraded to delta candidates:
/// `data = BlobRef(uuid)` and `patch = Lazy { old: BlobRef(uuid), new: FilePath(...) }`.
/// The patch will be computed in stage 7 with the blob as the delta base.
pub async fn s3_lookup(
    mut draft: FsDraft,
    storage: &dyn Storage,
    base_image_id: &str,
    partition_number: Option<i32>,
) -> Result<FsDraft> {
    let candidates = storage
        .find_blob_candidates(base_image_id, partition_number)
        .await?;
    if candidates.is_empty() {
        debug!("s3_lookup: no candidates in storage for base_image_id={base_image_id}");
        return Ok(draft);
    }
    debug!(
        base_image_id,
        candidates = candidates.len(),
        "s3_lookup: candidate blobs available"
    );

    // Paths of added files that still have a LazyBlob.
    let target_paths: Vec<String> = draft
        .records
        .iter()
        .filter(|r| {
            r.old_path.is_none()
                && r.entry_type == EntryType::File
                && matches!(r.data, Some(Data::LazyBlob(_)))
        })
        .filter_map(|r| r.new_path.clone())
        .collect();

    if target_paths.is_empty() {
        return Ok(draft);
    }

    let source_paths: Vec<String> = candidates.iter().map(|c| c.original_path.clone()).collect();
    let matches = find_best_matches(&source_paths, &target_paths, &PathMatchConfig::default())?;

    // Build lookup: target_path → (blob_id, sha256).
    let match_map: HashMap<&str, uuid::Uuid> = matches
        .iter()
        .filter_map(|m| {
            candidates
                .iter()
                .find(|c| c.original_path == m.source_path)
                .map(|c| (m.target_path.as_str(), c.uuid))
        })
        .collect();

    for record in &mut draft.records {
        let new_path = match &record.new_path {
            Some(p) => p.clone(),
            None => continue,
        };
        if let Some(&blob_id) = match_map.get(new_path.as_str()) {
            if let Some(Data::LazyBlob(lazy_path)) = &record.data {
                let lazy_path = lazy_path.clone();
                let blob_ref = BlobRef {
                    blob_id,
                    size: record.size,
                };
                debug!(
                    path = %new_path,
                    blob_id = %blob_id,
                    "s3_lookup: matched to base blob"
                );
                record.patch = Some(Patch::Lazy {
                    old_data: DataRef::BlobRef(blob_ref.clone()),
                    new_data: DataRef::FilePath(lazy_path),
                });
                record.data = Some(Data::BlobRef(blob_ref));
            }
        }
    }

    Ok(draft)
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 3 — match_renamed
// ─────────────────────────────────────────────────────────────────────────────

/// Detect file renames by matching removed files against added `LazyBlob` files.
///
/// Files already matched by [`s3_lookup`] (i.e. with `data = BlobRef`) are not
/// considered as rename targets to avoid breaking an already-optimal delta base.
///
/// A matched pair is collapsed into a single renamed record with:
/// `old_path = removed`, `new_path = added`,
/// `patch = Lazy { old: FilePath(base/old), new: FilePath(target/new) }`.
pub fn match_renamed(mut draft: FsDraft) -> FsDraft {
    // Collect candidate (index, path) pairs.
    let removed: Vec<(usize, String)> = draft
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.new_path.is_none() && r.entry_type == EntryType::File)
        .map(|(i, r)| (i, r.old_path.clone().unwrap()))
        .collect();

    let added: Vec<(usize, String)> = draft
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.old_path.is_none()
                && r.entry_type == EntryType::File
                && matches!(r.data, Some(Data::LazyBlob(_)))
        })
        .map(|(i, r)| (i, r.new_path.clone().unwrap()))
        .collect();

    if removed.is_empty() || added.is_empty() {
        return draft;
    }

    let removed_paths: Vec<String> = removed.iter().map(|(_, p)| p.clone()).collect();
    let added_paths: Vec<String> = added.iter().map(|(_, p)| p.clone()).collect();

    // For rename detection we must not penalise cross-directory matches: a
    // directory rename legitimately moves every file to a new first component.
    // first_component_weight is kept at 0.0 here (the default 5.0 is designed
    // for the delta-base lookup stage, where same-directory matches are
    // preferred; here it would cause all directory-rename pairs to fall below
    // the min_score threshold).
    let rename_match_config = PathMatchConfig {
        first_component_weight: 0.0,
        ..PathMatchConfig::default()
    };
    let matches = match find_best_matches(&removed_paths, &added_paths, &rename_match_config) {
        Ok(m) => m,
        Err(_) => return draft,
    };

    let mut remove_indices: Vec<usize> = Vec::new();
    let mut new_records: Vec<Record> = Vec::new();

    for m in &matches {
        let Some((rem_idx, old_path)) = removed.iter().find(|(_, p)| *p == m.source_path) else {
            continue;
        };
        let Some((add_idx, new_path)) = added.iter().find(|(_, p)| *p == m.target_path) else {
            continue;
        };

        let new_data_path = match &draft.records[*add_idx].data {
            Some(Data::LazyBlob(p)) => DataRef::FilePath(p.clone()),
            _ => continue,
        };
        let old_data_path = match &draft.records[*rem_idx].data {
            Some(Data::OriginalFile(p)) => DataRef::FilePath(p.clone()),
            _ => continue,
        };

        let size = draft.records[*add_idx].size;
        // Carry the target-file metadata from the "added" record so the
        // decompressor restores mode/mtime/uid/gid on the renamed file.
        let metadata = draft.records[*add_idx].metadata.clone();
        new_records.push(Record {
            old_path: Some(old_path.clone()),
            new_path: Some(new_path.clone()),
            entry_type: EntryType::File,
            size,
            data: None,
            patch: Some(Patch::Lazy {
                old_data: old_data_path,
                new_data: new_data_path,
            }),
            metadata,
        });

        remove_indices.push(*rem_idx);
        remove_indices.push(*add_idx);
    }

    // Remove matched records (reverse order to preserve lower indices).
    remove_indices.sort_unstable();
    remove_indices.dedup();
    for &i in remove_indices.iter().rev() {
        draft.records.swap_remove(i);
    }
    draft.records.extend(new_records);

    draft
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 4 — cleanup
// ─────────────────────────────────────────────────────────────────────────────

/// Finalise deletion records.
///
/// After rename matching, any remaining `new_path = None` records are true
/// deletions.  Their `data`, `patch`, and `metadata` fields are cleared — the
/// decompressor only needs `old_path` to know what to remove.
pub fn cleanup(mut draft: FsDraft) -> FsDraft {
    for record in &mut draft.records {
        if record.new_path.is_none() {
            record.data = None;
            record.patch = None;
            record.metadata = None;
        }
    }
    draft
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 5 — upload_lazy_blobs
// ─────────────────────────────────────────────────────────────────────────────

/// Upload all `Data::LazyBlob` files to storage, replacing them with
/// `Data::BlobRef`.
///
/// SHA-256 deduplication: if a blob with the same content already exists (same
/// SHA-256 digest), the existing UUID is reused and no bytes are transferred.
pub async fn upload_lazy_blobs(
    mut draft: FsDraft,
    storage: &dyn Storage,
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

        // Check before uploading to avoid a redundant network PUT for
        // content that is already stored (SHA-256 dedup).
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

// ─────────────────────────────────────────────────────────────────────────────
// Stage 6 — download_blobs_for_patches
// ─────────────────────────────────────────────────────────────────────────────

/// Download blobs referenced as `DataRef::BlobRef` in `Patch::Lazy` records to
/// `tmp_dir`, replacing them with `DataRef::FilePath`.
///
/// After this stage every `Patch::Lazy` has both `old_data` and `new_data` as
/// `FilePath`s, so [`compute_patches`] can read them from disk without async I/O.
///
/// Downloaded files are recorded in [`FsDraft::tmp_files`] so the caller can
/// clean them up after patching.  Blobs referenced by multiple records are
/// downloaded only once (in-memory dedup within the call).
pub async fn download_blobs_for_patches(
    mut draft: FsDraft,
    storage: &dyn Storage,
    tmp_dir: &Path,
) -> Result<FsDraft> {
    let mut blob_cache: HashMap<uuid::Uuid, PathBuf> = HashMap::new();

    for record in &mut draft.records {
        let old_data = match &mut record.patch {
            Some(Patch::Lazy { old_data, .. }) => old_data,
            _ => continue,
        };
        let blob_id = match old_data {
            DataRef::BlobRef(br) => br.blob_id,
            DataRef::FilePath(_) => continue,
        };

        let tmp_path = if let Some(p) = blob_cache.get(&blob_id) {
            p.clone()
        } else {
            let bytes = storage.download_blob(blob_id).await?;
            let p = tmp_dir.join(blob_id.to_string());
            std::fs::write(&p, &bytes)?;
            draft.tmp_files.push(p.clone());
            blob_cache.insert(blob_id, p.clone());
            p
        };

        *old_data = DataRef::FilePath(tmp_path);
    }
    Ok(draft)
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 7 — compute_patches
// ─────────────────────────────────────────────────────────────────────────────

/// Compute all binary patches in parallel (rayon) and populate
/// [`FsDraft::patch_bytes`].
///
/// For each record with `Patch::Lazy { old_data: FilePath, new_data: FilePath }`:
/// - Reads source and target bytes from disk.
/// - Encodes via `router` (symlinks and hardlinks always use
///   [`PassthroughEncoder`] to store the new link target verbatim).
/// - Stores raw patch bytes in `draft.patch_bytes` under the archive-entry name.
/// - Replaces `Patch::Lazy` with `Patch::Real`.
///
/// # Errors
///
/// Returns the first encoding error.  Other patches may have been computed when
/// the error is propagated.
pub fn compute_patches(
    mut draft: FsDraft,
    router: &RouterEncoder,
    workers: usize,
) -> Result<FsDraft> {
    let needs_patch: Vec<usize> = draft
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.patch, Some(Patch::Lazy { .. })))
        .map(|(i, _)| i)
        .collect();

    if needs_patch.is_empty() {
        return Ok(draft);
    }

    let passthrough: Arc<dyn PatchEncoder> = Arc::new(PassthroughEncoder::new());

    // Build a dedicated rayon pool sized to `workers` so that parallelism is
    // bounded by the caller-supplied value rather than the global rayon pool.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|e| crate::Error::Other(format!("failed to build rayon pool: {e}")))?;

    // Phase 1: compute patches in parallel (immutable borrow of records).
    type PatchResult = Result<(usize, PatchRef, Vec<u8>)>;
    let results: Vec<PatchResult> = pool.install(|| {
        needs_patch
            .par_iter()
            .map(|&i| {
                let record = &draft.records[i];
                let (old_data, new_data) = match &record.patch {
                    Some(Patch::Lazy { old_data, new_data }) => (old_data, new_data),
                    _ => unreachable!(),
                };

                let old_bytes = read_entry_bytes(old_data, &record.entry_type)?;
                let new_bytes = read_entry_bytes(new_data, &record.entry_type)?;

                let new_path_str = record.new_path.as_deref().unwrap_or("");
                let header_slice: &[u8] = &new_bytes[..new_bytes.len().min(16)];

                let encoder: Arc<dyn PatchEncoder> =
                    if matches!(record.entry_type, EntryType::Symlink | EntryType::Hardlink) {
                        Arc::clone(&passthrough)
                    } else {
                        router.select(&FileInfo {
                            path: new_path_str,
                            size: new_bytes.len() as u64,
                            header: header_slice,
                        })
                    };

                let base_snap = FileSnapshot {
                    path: record.old_path.as_deref().unwrap_or(""),
                    size: old_bytes.len() as u64,
                    header: &old_bytes[..old_bytes.len().min(16)],
                    bytes: &old_bytes,
                };
                let target_snap = FileSnapshot {
                    path: new_path_str,
                    size: new_bytes.len() as u64,
                    header: header_slice,
                    bytes: &new_bytes,
                };

                let file_patch = encoder.encode(&base_snap, &target_snap)?;

                let sha256 = hex_sha256_bytes(&file_patch.bytes);
                let archive_entry = format!("{:06}.patch", i);
                let pref = PatchRef {
                    archive_entry: archive_entry.clone(),
                    sha256,
                    algorithm_code: file_patch.code,
                    algorithm_id: file_patch.algorithm_id.clone(),
                };

                debug!(
                    path = new_path_str,
                    algorithm = file_patch.algorithm_id.as_deref().unwrap_or("unknown"),
                    patch_bytes = file_patch.bytes.len(),
                    "patch computed"
                );

                Ok((i, pref, file_patch.bytes))
            })
            .collect()
    });

    // Phase 2: apply results sequentially (mutable borrow).
    for res in results {
        let (idx, pref, bytes) = res?;
        let key = pref.archive_entry.clone();
        draft.records[idx].patch = Some(Patch::Real(pref));
        draft.patch_bytes.insert(key, bytes);
    }

    Ok(draft)
}

/// Read the bytes that represent `data` for patch encoding.
///
/// For symlinks the "bytes" are the UTF-8 encoded link-target string, not
/// the file content (symlinks are not regular files).
fn read_entry_bytes(data: &DataRef, entry_type: &EntryType) -> Result<Vec<u8>> {
    match data {
        DataRef::FilePath(path) => {
            if *entry_type == EntryType::Symlink {
                Ok(std::fs::read_link(path)?
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes())
            } else {
                Ok(std::fs::read(path)?)
            }
        }
        DataRef::BlobRef(_) => Err(crate::Error::Encode(
            "BlobRef in compute_patches: call download_blobs_for_patches first".into(),
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 8 — pack_and_upload_archive
// ─────────────────────────────────────────────────────────────────────────────

/// Pack all patch bytes into a tar archive, optionally gzip-compress it if
/// that saves space, upload it to storage, and return the final
/// [`PartitionContent::Fs`].
///
/// After this function the `draft.patch_bytes` map is consumed (cleared) to
/// release memory.
pub async fn pack_and_upload_archive(
    mut draft: FsDraft,
    storage: &dyn Storage,
    image_id: &str,
    fs_type: &str,
) -> Result<(PartitionContent, bool)> {
    // Build tar archive in memory.
    let tar_bytes = {
        let mut builder = tar::Builder::new(Vec::<u8>::new());
        // Sort for deterministic archive order.
        let mut entries: Vec<(String, Vec<u8>)> = draft.patch_bytes.drain().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, &name, bytes.as_slice())
                .map_err(|e| crate::Error::Archive(format!("tar append: {e}")))?;
        }
        builder
            .into_inner()
            .map_err(|e| crate::Error::Archive(format!("tar finish: {e}")))?
    };

    // Try gzip: use compressed version only if it is actually smaller.
    let (archive_bytes, compressed) = try_gzip(tar_bytes)?;

    // Update manifest header flag on all records' PatchRefs (nothing to do —
    // `patches_compressed` lives in ManifestHeader and is set by the orchestrator).
    storage
        .upload_patches(image_id, &archive_bytes, compressed)
        .await?;

    // Collect finalised records (filter out any accidental Lazy patches that
    // slipped through — should never happen in a correct pipeline run).
    let records: Vec<Record> = draft
        .records
        .into_iter()
        .filter(|r| !matches!(r.patch, Some(Patch::Lazy { .. })))
        .collect();

    Ok((
        PartitionContent::Fs {
            fs_type: fs_type.to_string(),
            records,
        },
        compressed,
    ))
}

/// Attempt to gzip `bytes`.  Returns `(bytes, true)` if the compressed form is
/// smaller, `(original, false)` otherwise.
fn try_gzip(bytes: Vec<u8>) -> Result<(Vec<u8>, bool)> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&bytes)
        .map_err(|e| crate::Error::Archive(format!("gzip write: {e}")))?;
    let compressed = enc
        .finish()
        .map_err(|e| crate::Error::Archive(format!("gzip finish: {e}")))?;

    if compressed.len() < bytes.len() {
        Ok((compressed, true))
    } else {
        Ok((bytes, false))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Orchestrator
// ─────────────────────────────────────────────────────────────────────────────

/// Run the full 8-stage compress pipeline for one Fs partition.
///
/// Chains stages 1–8 and returns a [`PartitionManifest`] that is ready to be
/// embedded in the top-level [`Manifest`].
///
/// Temporary files (downloaded delta-base blobs) are cleaned up after stage 7.
///
/// [`Manifest`]: crate::Manifest
#[allow(clippy::too_many_arguments)]
/// Returns `(PartitionManifest, patches_compressed)` where `patches_compressed`
/// is `true` when the uploaded patches archive is gzip-compressed (i.e.
/// compression reduced the archive size).
pub async fn compress_fs_partition(
    base_root: &Path,
    target_root: &Path,
    descriptor: &PartitionDescriptor,
    storage: &dyn Storage,
    image_id: &str,
    base_image_id: Option<&str>,
    router: &RouterEncoder,
    fs_type: &str,
    workers: usize,
) -> Result<(PartitionManifest, bool)> {
    let tmp_dir = tempfile::TempDir::new()?;

    info!(
        image_id,
        base_image_id,
        partition = descriptor.number,
        "stage 1/8: walkdir"
    );
    let mut draft = walkdir(base_root, target_root)?;
    let n_records = draft.records.len();
    info!(
        image_id,
        partition = descriptor.number,
        records = n_records,
        "stage 1/8: walkdir done"
    );

    // Stage 2.
    if let Some(base_id) = base_image_id {
        info!(
            image_id,
            base_image_id = base_id,
            partition = descriptor.number,
            "stage 2/8: s3_lookup"
        );
        let pn = Some(descriptor.number as i32);
        draft = s3_lookup(draft, storage, base_id, pn).await?;
        info!(
            image_id,
            partition = descriptor.number,
            "stage 2/8: s3_lookup done"
        );
    }

    info!(
        image_id,
        partition = descriptor.number,
        "stage 3/8: match_renamed"
    );
    draft = match_renamed(draft);
    info!(
        image_id,
        partition = descriptor.number,
        "stage 3/8: match_renamed done"
    );

    info!(
        image_id,
        partition = descriptor.number,
        "stage 4/8: cleanup"
    );
    draft = cleanup(draft);

    info!(
        image_id,
        partition = descriptor.number,
        "stage 5/8: upload_lazy_blobs"
    );
    draft = upload_lazy_blobs(
        draft,
        storage,
        image_id,
        base_image_id,
        Some(descriptor.number as i32),
    )
    .await?;

    info!(
        image_id,
        partition = descriptor.number,
        "stage 6/8: download_blobs_for_patches"
    );
    draft = download_blobs_for_patches(draft, storage, tmp_dir.path()).await?;

    let n_patches = draft
        .records
        .iter()
        .filter(|r| matches!(r.patch, Some(crate::manifest::Patch::Lazy { .. })))
        .count();
    info!(
        image_id,
        partition = descriptor.number,
        patches = n_patches,
        "stage 7/8: compute_patches"
    );
    draft = compute_patches(draft, router, workers)?;

    // Clean up downloaded tmp files now that patches are computed.
    for p in &draft.tmp_files {
        let _ = std::fs::remove_file(p);
    }
    draft.tmp_files.clear();

    info!(
        image_id,
        partition = descriptor.number,
        "stage 8/8: pack_and_upload_archive"
    );
    let (content, patches_compressed) =
        pack_and_upload_archive(draft, storage, image_id, fs_type).await?;

    info!(
        image_id,
        partition = descriptor.number,
        patches_compressed,
        "pipeline complete"
    );

    Ok((
        PartitionManifest {
            descriptor: descriptor.clone(),
            content,
        },
        patches_compressed,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn hex_sha256_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests — pure stages (no async, no external deps)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlgorithmCode;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn write(dir: &Path, rel: &str, content: &[u8]) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    fn mkdir(dir: &Path, rel: &str) {
        std::fs::create_dir_all(dir.join(rel)).unwrap();
    }

    #[allow(dead_code)]
    fn assert_has_record(draft: &FsDraft, old: Option<&str>, new: Option<&str>) -> &'static str {
        let found = draft
            .records
            .iter()
            .any(|r| r.old_path.as_deref() == old && r.new_path.as_deref() == new);
        assert!(
            found,
            "expected record old={old:?} new={new:?}; got {:#?}",
            draft.records
        );
        "ok"
    }

    // ── Stage 1: walkdir ──────────────────────────────────────────────────────

    #[test]
    fn test_walkdir_added_file() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(target.path(), "usr/bin/newcmd", b"binary content");

        let draft = walkdir(base.path(), target.path()).unwrap();

        let r = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some("usr/bin/newcmd"))
            .unwrap();
        assert_eq!(r.old_path, None);
        assert_eq!(r.entry_type, EntryType::File);
        assert!(matches!(r.data, Some(Data::LazyBlob(_))));
        assert!(r.patch.is_none());
    }

    #[test]
    fn test_walkdir_removed_file() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(base.path(), "etc/removed.conf", b"old content");

        let draft = walkdir(base.path(), target.path()).unwrap();

        let r = draft
            .records
            .iter()
            .find(|r| r.old_path.as_deref() == Some("etc/removed.conf"))
            .unwrap();
        assert_eq!(r.new_path, None);
        assert_eq!(r.entry_type, EntryType::File);
        assert!(matches!(r.data, Some(Data::OriginalFile(_))));
    }

    #[test]
    fn test_walkdir_changed_file() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(base.path(), "lib/libfoo.so.1", b"version 1.0 content here");
        write(
            target.path(),
            "lib/libfoo.so.1",
            b"version 1.1 content here",
        );

        let draft = walkdir(base.path(), target.path()).unwrap();

        let r = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some("lib/libfoo.so.1"))
            .unwrap();
        assert_eq!(r.old_path.as_deref(), Some("lib/libfoo.so.1"));
        assert_eq!(r.entry_type, EntryType::File);
        assert!(r.data.is_none());
        assert!(matches!(r.patch, Some(Patch::Lazy { .. })));
    }

    #[test]
    fn test_walkdir_unchanged_file_not_recorded() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        let content = b"identical content";
        write(base.path(), "etc/same.conf", content);
        write(target.path(), "etc/same.conf", content);
        // Align mtime so metadata doesn't cause a record.
        let src_mtime = base
            .path()
            .join("etc/same.conf")
            .symlink_metadata()
            .unwrap()
            .mtime();
        filetime::set_file_mtime(
            target.path().join("etc/same.conf"),
            filetime::FileTime::from_unix_time(src_mtime, 0),
        )
        .unwrap();

        let draft = walkdir(base.path(), target.path()).unwrap();

        assert!(
            draft
                .records
                .iter()
                .all(|r| r.new_path.as_deref() != Some("etc/same.conf")),
            "unchanged file should not produce a record"
        );
    }

    #[test]
    fn test_walkdir_added_directory() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        mkdir(target.path(), "usr/share/newpkg");

        let draft = walkdir(base.path(), target.path()).unwrap();

        let r = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some("usr/share/newpkg"))
            .unwrap();
        assert_eq!(r.entry_type, EntryType::Directory);
        assert!(r.data.is_none());
        assert!(r.patch.is_none());
        assert!(r.metadata.is_some());
    }

    #[test]
    fn test_walkdir_added_symlink() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        std::fs::create_dir_all(target.path().join("usr/bin")).unwrap();
        symlink("/usr/bin/python3.11", target.path().join("usr/bin/python")).unwrap();

        let draft = walkdir(base.path(), target.path()).unwrap();

        let r = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some("usr/bin/python"))
            .unwrap();
        assert_eq!(r.entry_type, EntryType::Symlink);
        assert!(matches!(r.data, Some(Data::SoftlinkTo(ref t)) if t == "/usr/bin/python3.11"));
        assert!(r.patch.is_none());
    }

    #[test]
    fn test_walkdir_changed_symlink() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        std::fs::create_dir_all(base.path().join("usr/bin")).unwrap();
        std::fs::create_dir_all(target.path().join("usr/bin")).unwrap();
        symlink("/usr/bin/python3.10", base.path().join("usr/bin/python")).unwrap();
        symlink("/usr/bin/python3.11", target.path().join("usr/bin/python")).unwrap();

        let draft = walkdir(base.path(), target.path()).unwrap();

        let r = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some("usr/bin/python"))
            .unwrap();
        assert_eq!(r.entry_type, EntryType::Symlink);
        assert!(r.data.is_none());
        // Changed symlink → Lazy patch (will be encoded with PassthroughEncoder).
        assert!(matches!(r.patch, Some(Patch::Lazy { .. })));
    }

    #[test]
    fn test_walkdir_type_conflict_produces_two_records() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        // base: directory; target: file at same path → conflict
        mkdir(base.path(), "etc/foo");
        write(target.path(), "etc/foo", b"now a file");

        let draft = walkdir(base.path(), target.path()).unwrap();

        let removal = draft
            .records
            .iter()
            .find(|r| r.old_path.as_deref() == Some("etc/foo") && r.new_path.is_none());
        let addition = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some("etc/foo") && r.old_path.is_none());
        assert!(removal.is_some(), "expected a deletion record for etc/foo");
        assert!(
            addition.is_some(),
            "expected an addition record for etc/foo"
        );
    }

    #[test]
    fn test_walkdir_dir_metadata_only() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        mkdir(base.path(), "etc/ssl");
        mkdir(target.path(), "etc/ssl");
        // Change mode on target dir.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            target.path().join("etc/ssl"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();

        let draft = walkdir(base.path(), target.path()).unwrap();

        // May or may not produce a record depending on whether base mode == 0o700.
        // Just verify that if a record exists it has no patch and no data.
        for r in draft.records.iter().filter(|r| {
            r.old_path.as_deref() == Some("etc/ssl") && r.new_path.as_deref() == Some("etc/ssl")
        }) {
            assert!(r.data.is_none());
            assert!(r.patch.is_none());
            assert!(r.metadata.is_some());
        }
    }

    #[test]
    fn test_walkdir_hardlink() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        // Canonical is alphabetically first: "usr/share/common-licenses/GPL-2"
        // comes before "usr/share/licenses/GPL-2".
        let first_path = target.path().join("usr/share/common-licenses/GPL-2");
        write(
            target.path(),
            "usr/share/common-licenses/GPL-2",
            b"GPL license text",
        );
        let second_path = target.path().join("usr/share/licenses/GPL-2");
        std::fs::create_dir_all(second_path.parent().unwrap()).unwrap();
        std::fs::hard_link(&first_path, &second_path).unwrap();

        let draft = walkdir(base.path(), target.path()).unwrap();

        // Canonical = alphabetically first = common-licenses → LazyBlob.
        let canonical_rec = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some("usr/share/common-licenses/GPL-2"))
            .unwrap();
        assert!(matches!(canonical_rec.data, Some(Data::LazyBlob(_))));

        // Non-canonical = licenses → HardlinkTo(canonical).
        let link_rec = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some("usr/share/licenses/GPL-2"))
            .unwrap();
        assert!(
            matches!(&link_rec.data, Some(Data::HardlinkTo(p)) if p == "usr/share/common-licenses/GPL-2"),
            "expected HardlinkTo canonical path, got {:?}",
            link_rec.data
        );
    }

    // ── Stage 3: match_renamed ────────────────────────────────────────────────

    fn lazy_blob_record(old: Option<&str>, new: Option<&str>, path: &str) -> Record {
        Record {
            old_path: old.map(|s| s.to_string()),
            new_path: new.map(|s| s.to_string()),
            entry_type: EntryType::File,
            size: 100,
            data: if new.is_some() {
                Some(Data::LazyBlob(PathBuf::from(format!("/mnt/target/{path}"))))
            } else {
                Some(Data::OriginalFile(PathBuf::from(format!(
                    "/mnt/base/{path}"
                ))))
            },
            patch: None,
            metadata: None,
        }
    }

    #[test]
    fn test_match_renamed_basic() {
        let mut draft = FsDraft::default();
        // Removed: lib/libfoo.so.1
        draft.records.push(lazy_blob_record(
            Some("lib/libfoo.so.1"),
            None,
            "lib/libfoo.so.1",
        ));
        // Added: lib/libfoo.so.2 (same dir, different version → high score)
        draft.records.push(lazy_blob_record(
            None,
            Some("lib/libfoo.so.2"),
            "lib/libfoo.so.2",
        ));

        let draft = match_renamed(draft);

        // After matching: one renamed record, no orphan added/removed.
        let renamed = draft.records.iter().find(|r| {
            r.old_path.as_deref() == Some("lib/libfoo.so.1")
                && r.new_path.as_deref() == Some("lib/libfoo.so.2")
        });
        assert!(renamed.is_some(), "expected a renamed record");
        assert!(
            matches!(renamed.unwrap().patch, Some(Patch::Lazy { .. })),
            "renamed record should have a Lazy patch"
        );
        // Original add/remove records should be gone.
        assert!(
            !draft
                .records
                .iter()
                .any(|r| r.old_path.as_deref() == Some("lib/libfoo.so.1") && r.new_path.is_none()),
            "orphan remove record should be consumed"
        );
    }

    #[test]
    fn test_match_renamed_s3_matched_not_considered() {
        let mut draft = FsDraft::default();
        // Removed file
        draft.records.push(lazy_blob_record(
            Some("lib/libfoo.so.1"),
            None,
            "lib/libfoo.so.1",
        ));
        // Added file that was ALREADY matched by s3_lookup (BlobRef, not LazyBlob).
        draft.records.push(Record {
            old_path: None,
            new_path: Some("lib/libfoo.so.2".into()),
            entry_type: EntryType::File,
            size: 100,
            data: Some(Data::BlobRef(BlobRef {
                blob_id: uuid::Uuid::nil(),
                size: 100,
            })),
            patch: Some(Patch::Lazy {
                old_data: DataRef::BlobRef(BlobRef {
                    blob_id: uuid::Uuid::nil(),
                    size: 100,
                }),
                new_data: DataRef::FilePath("/mnt/target/lib/libfoo.so.2".into()),
            }),
            metadata: None,
        });

        let before_count = draft.records.len();
        let draft = match_renamed(draft);

        // S3-matched record should NOT be consumed by rename matching.
        assert_eq!(
            draft.records.len(),
            before_count,
            "s3-matched record must not be consumed by rename matching"
        );
    }

    #[test]
    fn test_match_renamed_no_candidates_is_noop() {
        let mut draft = FsDraft::default();
        draft
            .records
            .push(lazy_blob_record(Some("etc/old.conf"), None, "etc/old.conf"));
        // No added files → nothing to match.
        let before_count = draft.records.len();
        let draft = match_renamed(draft);
        assert_eq!(draft.records.len(), before_count);
    }

    // ── Stage 4: cleanup ──────────────────────────────────────────────────────

    #[test]
    fn test_cleanup_clears_deletion_records() {
        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("etc/removed.conf".into()),
            new_path: None,
            entry_type: EntryType::File,
            size: 512,
            data: Some(Data::OriginalFile("/mnt/base/etc/removed.conf".into())),
            patch: None,
            metadata: Some(Metadata {
                mode: Some(0o644),
                ..Default::default()
            }),
        });

        let draft = cleanup(draft);

        let r = &draft.records[0];
        assert!(r.data.is_none(), "data should be cleared");
        assert!(r.patch.is_none(), "patch should be cleared");
        assert!(r.metadata.is_none(), "metadata should be cleared");
    }

    #[test]
    fn test_cleanup_does_not_touch_non_deletions() {
        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("etc/changed.conf".into()),
            new_path: Some("etc/changed.conf".into()),
            entry_type: EntryType::File,
            size: 512,
            data: None,
            patch: Some(Patch::Lazy {
                old_data: DataRef::FilePath("/mnt/base/etc/changed.conf".into()),
                new_data: DataRef::FilePath("/mnt/target/etc/changed.conf".into()),
            }),
            metadata: None,
        });

        let draft = cleanup(draft);

        assert!(
            matches!(draft.records[0].patch, Some(Patch::Lazy { .. })),
            "non-deletion record must not be modified"
        );
    }

    // ── Stage 7: compute_patches ──────────────────────────────────────────────

    fn make_xdelta3_router() -> RouterEncoder {
        use crate::Xdelta3Encoder;
        RouterEncoder::new(vec![], Arc::new(Xdelta3Encoder::new()))
    }

    #[test]
    fn test_compute_patches_xdelta3() {
        let base_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();
        write(base_dir.path(), "lib/libz.so.1", b"base content of libz");
        write(
            target_dir.path(),
            "lib/libz.so.1",
            b"updated content of libz v2",
        );

        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("lib/libz.so.1".into()),
            new_path: Some("lib/libz.so.1".into()),
            entry_type: EntryType::File,
            size: 26,
            data: None,
            patch: Some(Patch::Lazy {
                old_data: DataRef::FilePath(base_dir.path().join("lib/libz.so.1")),
                new_data: DataRef::FilePath(target_dir.path().join("lib/libz.so.1")),
            }),
            metadata: None,
        });

        let router = make_xdelta3_router();
        let draft = compute_patches(draft, &router, 4).unwrap();

        let record = &draft.records[0];
        assert!(
            matches!(record.patch, Some(Patch::Real(_))),
            "Lazy patch should become Real after compute_patches"
        );
        let pref = match &record.patch {
            Some(Patch::Real(p)) => p,
            _ => unreachable!(),
        };
        assert_eq!(pref.algorithm_code, AlgorithmCode::Xdelta3);
        assert!(!pref.sha256.is_empty());
        assert!(
            draft.patch_bytes.contains_key(&pref.archive_entry),
            "patch bytes must be stored"
        );
    }

    #[test]
    fn test_compute_patches_passthrough_symlink() {
        let base_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(base_dir.path().join("usr/bin")).unwrap();
        std::fs::create_dir_all(target_dir.path().join("usr/bin")).unwrap();
        symlink(
            "/usr/bin/python3.10",
            base_dir.path().join("usr/bin/python"),
        )
        .unwrap();
        symlink(
            "/usr/bin/python3.11",
            target_dir.path().join("usr/bin/python"),
        )
        .unwrap();

        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("usr/bin/python".into()),
            new_path: Some("usr/bin/python".into()),
            entry_type: EntryType::Symlink,
            size: 0,
            data: None,
            patch: Some(Patch::Lazy {
                old_data: DataRef::FilePath(base_dir.path().join("usr/bin/python")),
                new_data: DataRef::FilePath(target_dir.path().join("usr/bin/python")),
            }),
            metadata: None,
        });

        let router = make_xdelta3_router();
        let draft = compute_patches(draft, &router, 4).unwrap();

        let pref = match &draft.records[0].patch {
            Some(Patch::Real(p)) => p,
            _ => panic!("expected Patch::Real"),
        };
        assert_eq!(
            pref.algorithm_code,
            AlgorithmCode::Passthrough,
            "symlink patch must use passthrough encoder"
        );

        // Verify: decoded patch = new link target bytes.
        let patch_bytes = &draft.patch_bytes[&pref.archive_entry];
        let decoded = String::from_utf8(patch_bytes.clone()).unwrap();
        assert_eq!(decoded, "/usr/bin/python3.11");
    }

    #[test]
    fn test_compute_patches_no_lazy_patches_is_noop() {
        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("etc/removed".into()),
            new_path: None,
            entry_type: EntryType::File,
            size: 0,
            data: None,
            patch: None,
            metadata: None,
        });

        let router = make_xdelta3_router();
        let result = compute_patches(draft, &router, 1).unwrap();

        assert!(result.patch_bytes.is_empty());
    }
}
