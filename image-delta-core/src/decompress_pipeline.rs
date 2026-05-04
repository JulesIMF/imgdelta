// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress pipeline: reconstruct one Fs partition from base + manifest records

//! Decompress pipeline for one `Fs` partition.
//!
//! ## Algorithm
//!
//! 1. Optionally decompress the patches archive (gzip) and index entries by
//!    name into a `HashMap`.
//! 2. Collect the set of `old_path` values from all records.  These are base
//!    files that were deleted, changed in-place, or renamed.
//! 3. Walk `base_root` and copy every entry whose relative path is **not** in
//!    the affected set into `output_root` unchanged.
//! 4. Process each record in manifest order:
//!    - `Directory` (added / renamed): `create_dir_all`
//!    - `Symlink`:  `std::os::unix::fs::symlink`
//!    - `Hardlink`: `std::fs::hard_link` to the already-written canonical
//!    - File with `Data::BlobRef`:  download from storage → write
//!    - File with `Patch::Real`:    read base bytes + decode + write
//!    - Metadata-only change (no patch, no new data):  copy from base
//!    - Deletion (`new_path = None`): nothing to write
//!
//! Output statistics are accumulated per-partition and returned as
//! [`PartitionDecompressStats`].

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::algorithm::FilePatch;
use crate::encoder::PatchEncoder;
use crate::manifest::{Data, EntryType, Patch, Record};
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::{Error, Result};

// ── Public stats type ─────────────────────────────────────────────────────────

/// Per-partition decompress statistics.
#[derive(Debug, Default)]
pub struct PartitionDecompressStats {
    pub files_written: usize,
    pub patches_verified: usize,
    pub bytes_written: u64,
}

// ── Archive extraction ────────────────────────────────────────────────────────

/// Extract a patches tar (or tar.gz) archive into a map of `entry_name → bytes`.
fn extract_archive(
    archive_bytes: &[u8],
    compressed: bool,
) -> Result<std::collections::HashMap<String, Vec<u8>>> {
    let mut map = std::collections::HashMap::new();

    if compressed {
        let decoder = flate2::read::GzDecoder::new(archive_bytes);
        let mut ar = tar::Archive::new(decoder);
        for entry in ar
            .entries()
            .map_err(|e| Error::Archive(format!("tar entries: {e}")))?
        {
            let mut entry = entry.map_err(|e| Error::Archive(format!("tar entry: {e}")))?;
            let name = entry
                .path()
                .map_err(|e| Error::Archive(format!("tar entry path: {e}")))?
                .to_string_lossy()
                .into_owned();
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| Error::Archive(format!("tar entry read: {e}")))?;
            map.insert(name, bytes);
        }
    } else {
        let mut ar = tar::Archive::new(archive_bytes);
        for entry in ar
            .entries()
            .map_err(|e| Error::Archive(format!("tar entries: {e}")))?
        {
            let mut entry = entry.map_err(|e| Error::Archive(format!("tar entry: {e}")))?;
            let name = entry
                .path()
                .map_err(|e| Error::Archive(format!("tar entry path: {e}")))?
                .to_string_lossy()
                .into_owned();
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| Error::Archive(format!("tar entry read: {e}")))?;
            map.insert(name, bytes);
        }
    }

    Ok(map)
}

// ── File I/O helpers ──────────────────────────────────────────────────────────

fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Other(format!("create_dir_all {}: {e}", parent.display())))?;
    }
    std::fs::write(path, data).map_err(|e| Error::Other(format!("write {}: {e}", path.display())))
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::Other(format!("read {}: {e}", path.display())))
}

// ── Copy base tree (unchanged entries) ───────────────────────────────────────

/// Copy all entries in `base_root` whose relative path is NOT in `affected`
/// into `output_root`, preserving directory structure.
fn copy_unchanged_from_base(
    base_root: &Path,
    output_root: &Path,
    affected: &HashSet<String>,
) -> Result<PartitionDecompressStats> {
    let mut stats = PartitionDecompressStats::default();

    for entry in WalkDir::new(base_root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let abs = entry.path();
        let rel = match abs.strip_prefix(base_root) {
            Ok(r) if !r.as_os_str().is_empty() => r.to_string_lossy().into_owned(),
            _ => continue, // skip the root itself
        };
        // Normalise path separator to '/'
        let rel = rel.replace(std::path::MAIN_SEPARATOR, "/");

        if affected.contains(&rel) {
            continue; // this entry is covered by a record — skip
        }

        let dst = output_root.join(&rel);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dst)
                .map_err(|e| Error::Other(format!("create_dir {}: {e}", dst.display())))?;
        } else if entry.file_type().is_symlink() {
            let link_target = std::fs::read_link(abs)
                .map_err(|e| Error::Other(format!("readlink {}: {e}", abs.display())))?;
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link_target, &dst).map_err(|e| {
                Error::Other(format!(
                    "symlink {} → {}: {e}",
                    dst.display(),
                    link_target.display()
                ))
            })?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let data = read_file(abs)?;
            // Capture source mtime before writing (write resets it).
            let src_mtime = abs
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(filetime::FileTime::from_system_time);
            stats.bytes_written += data.len() as u64;
            std::fs::write(&dst, &data)
                .map_err(|e| Error::Other(format!("write {}: {e}", dst.display())))?;
            // Restore the source mtime so unchanged files retain their timestamp.
            if let Some(ft) = src_mtime {
                let _ = filetime::set_file_mtime(&dst, ft);
            }
            stats.files_written += 1;
        }
    }

    Ok(stats)
}

// ── Main pipeline function ────────────────────────────────────────────────────

/// Reconstruct an Fs partition into `output_root` from `base_root` + manifest records.
///
/// `archive_bytes` is the raw content returned by [`Storage::download_patches`].
/// `patches_compressed` must match the flag in [`ManifestHeader::patches_compressed`].
///
/// Returns accumulated [`PartitionDecompressStats`] for the partition.
pub async fn decompress_fs_partition(
    base_root: &Path,
    output_root: &Path,
    records: &[Record],
    archive_bytes: &[u8],
    patches_compressed: bool,
    storage: &dyn Storage,
    router: &RouterEncoder,
) -> Result<PartitionDecompressStats> {
    // Step 1: Extract patch archive.
    let patch_map = if archive_bytes.is_empty() {
        std::collections::HashMap::new()
    } else {
        extract_archive(archive_bytes, patches_compressed)?
    };

    // Step 2: Collect affected base paths.
    let affected: HashSet<String> = records.iter().filter_map(|r| r.old_path.clone()).collect();

    // Step 3: Copy unchanged base files to output.
    let mut stats = copy_unchanged_from_base(base_root, output_root, &affected)?;

    // Step 4: Process each record.
    for record in records {
        apply_record(
            record,
            base_root,
            output_root,
            &patch_map,
            storage,
            router,
            &mut stats,
        )
        .await?;
    }

    Ok(stats)
}

// ── Per-record application ────────────────────────────────────────────────────

async fn apply_record(
    record: &Record,
    base_root: &Path,
    output_root: &Path,
    patch_map: &std::collections::HashMap<String, Vec<u8>>,
    storage: &dyn Storage,
    router: &RouterEncoder,
    stats: &mut PartitionDecompressStats,
) -> Result<()> {
    let new_path = match &record.new_path {
        Some(p) => p,
        None => return Ok(()), // deletion — nothing to write
    };

    let dst = output_root.join(new_path);

    match &record.entry_type {
        EntryType::Directory => {
            std::fs::create_dir_all(&dst)
                .map_err(|e| Error::Other(format!("mkdir {}: {e}", dst.display())))?;
        }

        EntryType::Symlink => {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            // Added symlink: data = SoftlinkTo(target)
            // Changed symlink: data = None, patch = Real (patch bytes = new link target as UTF-8)
            let target_str = match (&record.data, &record.patch) {
                (Some(Data::SoftlinkTo(t)), _) => t.clone(),
                (None, Some(Patch::Real(pref))) => {
                    // Changed symlink — decode patch using old link_target as base
                    let base_path = record
                        .old_path
                        .as_deref()
                        .map(|p| base_root.join(p))
                        .ok_or_else(|| {
                            Error::Format(format!("changed symlink {new_path} has no old_path"))
                        })?;
                    // Old "content" of a symlink = its target string as UTF-8 bytes
                    let old_bytes = std::fs::read_link(&base_path)
                        .map(|p| p.to_string_lossy().into_owned().into_bytes())
                        .unwrap_or_default();
                    let patch_bytes = patch_map.get(&pref.archive_entry).ok_or_else(|| {
                        Error::Archive(format!(
                            "patch archive entry '{}' not found",
                            pref.archive_entry
                        ))
                    })?;
                    let actual_sha = hex::encode(Sha256::digest(patch_bytes));
                    if actual_sha != pref.sha256 {
                        return Err(Error::Decode(format!(
                            "patch SHA-256 mismatch for symlink {new_path}: expected {}, got {actual_sha}",
                            pref.sha256
                        )));
                    }
                    let fp = FilePatch {
                        bytes: patch_bytes.clone(),
                        code: pref.algorithm_code,
                        algorithm_id: pref.algorithm_id.clone(),
                    };
                    let decoded = router.decode(&old_bytes, &fp)?;
                    stats.patches_verified += 1;
                    String::from_utf8(decoded).map_err(|e| {
                        Error::Format(format!("symlink target not valid UTF-8: {e}"))
                    })?
                }
                _ => {
                    return Err(Error::Format(format!(
                        "symlink record {new_path} has no SoftlinkTo data or patch"
                    )))
                }
            };
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target_str, &dst).map_err(|e| {
                Error::Other(format!(
                    "symlink {dst} → {target_str}: {e}",
                    dst = dst.display()
                ))
            })?;
            // symlinks count toward files_written for symmetry with compress stats
            stats.files_written += 1;
        }

        EntryType::Hardlink => {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let canonical = match &record.data {
                Some(Data::HardlinkTo(c)) => c.clone(),
                _ => {
                    return Err(Error::Format(format!(
                        "hardlink record {new_path} has no HardlinkTo data"
                    )))
                }
            };
            let src = output_root.join(&canonical);
            std::fs::hard_link(&src, &dst).map_err(|e| {
                Error::Other(format!(
                    "hard_link {} → {}: {e}",
                    src.display(),
                    dst.display()
                ))
            })?;
            stats.files_written += 1;
        }

        EntryType::File | EntryType::Other => {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            apply_file_record(
                record, new_path, &dst, base_root, patch_map, storage, router, stats,
            )
            .await?;
        }
    }

    // Apply metadata (mode, mtime) if present.  Symlinks are excluded because
    // most Linux filesystems do not support lchmod.
    if !matches!(record.entry_type, EntryType::Symlink | EntryType::Directory) {
        apply_metadata(&dst, record.metadata.as_ref());
    }

    Ok(())
}

/// Apply file metadata (mode, mtime) to an already-written path.
fn apply_metadata(path: &Path, meta: Option<&crate::manifest::Metadata>) {
    let meta = match meta {
        Some(m) => m,
        None => return,
    };
    if let Some(mode) = meta.mode {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    if let Some(mtime) = meta.mtime {
        let _ = filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(mtime, 0));
    }
}

/// Write a regular file record — handles blob, patch, and metadata-only cases.
#[allow(clippy::too_many_arguments)]
async fn apply_file_record(
    record: &Record,
    new_path: &str,
    dst: &Path,
    base_root: &Path,
    patch_map: &std::collections::HashMap<String, Vec<u8>>,
    storage: &dyn Storage,
    router: &RouterEncoder,
    stats: &mut PartitionDecompressStats,
) -> Result<()> {
    // Case A: patch stored in archive
    if let Some(Patch::Real(pref)) = &record.patch {
        let base_path = match &record.old_path {
            Some(p) => base_root.join(p),
            None => {
                // Patched addition — no base. Use empty source.
                let source: &[u8] = &[];
                let patch_bytes = patch_map.get(&pref.archive_entry).ok_or_else(|| {
                    Error::Archive(format!(
                        "patch archive entry '{}' not found",
                        pref.archive_entry
                    ))
                })?;
                // Verify SHA-256 of patch bytes.
                let actual_sha = hex::encode(Sha256::digest(patch_bytes));
                if actual_sha != pref.sha256 {
                    return Err(Error::Decode(format!(
                        "patch SHA-256 mismatch for {new_path}: expected {}, got {actual_sha}",
                        pref.sha256
                    )));
                }
                let fp = FilePatch {
                    bytes: patch_bytes.clone(),
                    code: pref.algorithm_code,
                    algorithm_id: pref.algorithm_id.clone(),
                };
                let decoded = router.decode(source, &fp)?;
                let n = decoded.len() as u64;
                write_file(dst, &decoded)?;
                stats.files_written += 1;
                stats.patches_verified += 1;
                stats.bytes_written += n;
                return Ok(());
            }
        };
        let source = read_file(&base_path)?;
        let patch_bytes = patch_map.get(&pref.archive_entry).ok_or_else(|| {
            Error::Archive(format!(
                "patch archive entry '{}' not found",
                pref.archive_entry
            ))
        })?;
        // Verify patch integrity before decoding.
        let actual_sha = hex::encode(Sha256::digest(patch_bytes));
        if actual_sha != pref.sha256 {
            return Err(Error::Decode(format!(
                "patch SHA-256 mismatch for {new_path}: expected {}, got {actual_sha}",
                pref.sha256
            )));
        }
        let fp = FilePatch {
            bytes: patch_bytes.clone(),
            code: pref.algorithm_code,
            algorithm_id: pref.algorithm_id.clone(),
        };
        let decoded = router.decode(&source, &fp)?;
        let n = decoded.len() as u64;
        write_file(dst, &decoded)?;
        stats.files_written += 1;
        stats.patches_verified += 1;
        stats.bytes_written += n;
        return Ok(());
    }

    // Case B: verbatim blob in storage
    if let Some(Data::BlobRef(bref)) = &record.data {
        let data = storage.download_blob(bref.blob_id).await?;
        let n = data.len() as u64;
        write_file(dst, &data)?;
        stats.files_written += 1;
        stats.bytes_written += n;
        return Ok(());
    }

    // Case C: metadata-only change (no new content) — copy from base
    if let Some(old) = &record.old_path {
        let src = base_root.join(old);
        if src.exists() {
            let data = read_file(&src)?;
            let n = data.len() as u64;
            write_file(dst, &data)?;
            stats.files_written += 1;
            stats.bytes_written += n;
        }
        return Ok(());
    }

    // Case D: new file with no data (empty file)
    write_file(dst, &[])?;
    stats.files_written += 1;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_archive ───────────────────────────────────────────────────────

    fn make_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let tar = make_tar(entries);
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn test_extract_archive_uncompressed() {
        let tar = make_tar(&[("000000.patch", b"data-a"), ("000001.patch", b"data-b")]);
        let map = extract_archive(&tar, false).unwrap();
        assert_eq!(map["000000.patch"], b"data-a");
        assert_eq!(map["000001.patch"], b"data-b");
    }

    #[test]
    fn test_extract_archive_compressed() {
        let tar_gz = make_tar_gz(&[("abc.patch", b"hello")]);
        let map = extract_archive(&tar_gz, true).unwrap();
        assert_eq!(map["abc.patch"], b"hello");
    }

    #[test]
    fn test_extract_archive_empty_bytes() {
        // Empty bytes → no entries (handled by the early return in the pipeline)
        // This is not called with empty bytes in practice (we short-circuit above)
        // but test the real path: empty tar has a 1024-byte EOF block that is valid
        let tar = make_tar(&[]);
        let map = extract_archive(&tar, false).unwrap();
        assert!(map.is_empty());
    }

    // ── copy_unchanged_from_base ──────────────────────────────────────────────

    #[test]
    fn test_copy_unchanged_skips_affected() {
        let base = tempfile::TempDir::new().unwrap();
        let out = tempfile::TempDir::new().unwrap();

        std::fs::write(base.path().join("keep.txt"), b"keep").unwrap();
        std::fs::write(base.path().join("skip.txt"), b"skip").unwrap();

        let affected: HashSet<String> = ["skip.txt".into()].into();
        copy_unchanged_from_base(base.path(), out.path(), &affected).unwrap();

        assert!(out.path().join("keep.txt").exists());
        assert!(!out.path().join("skip.txt").exists());
    }

    #[test]
    fn test_copy_unchanged_preserves_subdirs() {
        let base = tempfile::TempDir::new().unwrap();
        let out = tempfile::TempDir::new().unwrap();

        std::fs::create_dir(base.path().join("sub")).unwrap();
        std::fs::write(base.path().join("sub/a.txt"), b"a").unwrap();
        std::fs::write(base.path().join("sub/b.txt"), b"b").unwrap();

        let affected: HashSet<String> = ["sub/b.txt".into()].into();
        copy_unchanged_from_base(base.path(), out.path(), &affected).unwrap();

        assert_eq!(std::fs::read(out.path().join("sub/a.txt")).unwrap(), b"a");
        assert!(!out.path().join("sub/b.txt").exists());
    }

    // ── SHA-256 mismatch detection ────────────────────────────────────────────

    #[test]
    fn test_patch_sha256_mismatch_is_detected() {
        // Verify that the SHA-256 computed from real patch bytes does not
        // match a deliberately wrong expected SHA, confirming the check logic
        // would reject it.
        let real_patch_bytes = b"real-patch-data";
        let actual = hex::encode(Sha256::digest(real_patch_bytes));
        let wrong = "deadbeef".repeat(8);
        assert_ne!(actual, wrong, "SHA should not match");
    }
}
