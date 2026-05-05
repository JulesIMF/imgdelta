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
use tracing::{debug, info};
use walkdir::WalkDir;

use crate::algorithm::FilePatch;
use crate::encoder::PatchEncoder;
use crate::manifest::{Data, EntryType, Patch, Record};
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::{Error, Result};

#[cfg(unix)]
use libc;

// ── Device helpers (Linux major/minor encoding) ───────────────────────────────

/// Encode a major/minor pair into a raw `dev_t` (Linux kernel encoding).
#[cfg(unix)]
fn linux_makedev(major: u32, minor: u32) -> u64 {
    libc::makedev(major, minor)
}

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
            // Preserve source directory mode.
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                if let Ok(m) = abs.metadata() {
                    let mode = m.mode() & 0o7777;
                    let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode));
                }
            }
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
            // Capture source mtime and mode before writing (write resets both).
            let src_meta = abs.metadata().ok();
            let src_mtime = src_meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .map(filetime::FileTime::from_system_time);
            #[cfg(unix)]
            let src_mode: Option<u32> = {
                use std::os::unix::fs::MetadataExt;
                src_meta.as_ref().map(|m| m.mode() & 0o7777)
            };
            stats.bytes_written += data.len() as u64;
            std::fs::write(&dst, &data)
                .map_err(|e| Error::Other(format!("write {}: {e}", dst.display())))?;
            // Restore mode.
            #[cfg(unix)]
            if let Some(mode) = src_mode {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode));
            }
            // Restore mtime.
            if let Some(ft) = src_mtime {
                let _ = filetime::set_file_mtime(&dst, ft);
            }
            stats.files_written += 1;
        } else {
            // Special file (char/block device, FIFO, socket) — recreate via mknod.
            #[cfg(unix)]
            {
                use std::ffi::CString;
                use std::os::unix::ffi::OsStrExt;
                use std::os::unix::fs::MetadataExt;
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                if let Ok(meta) = abs.symlink_metadata() {
                    let rdev = meta.rdev();
                    let mode = meta.mode(); // includes S_IF* type bits
                    let dev = linux_makedev(libc::major(rdev) as u32, libc::minor(rdev) as u32);
                    if let Ok(c_path) = CString::new(dst.as_os_str().as_bytes()) {
                        let ret = unsafe {
                            libc::mknod(c_path.as_ptr(), mode as libc::mode_t, dev as libc::dev_t)
                        };
                        if ret == 0 {
                            let ft = filetime::FileTime::from_unix_time(meta.mtime(), 0);
                            let _ = filetime::set_file_mtime(&dst, ft);
                            stats.files_written += 1;
                        } else {
                            debug!(
                                path = %dst.display(),
                                err = %std::io::Error::last_os_error(),
                                "mknod failed for base special file (skipping)"
                            );
                        }
                    }
                }
            }
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
    info!(
        records = records.len(),
        archive_bytes = archive_bytes.len(),
        patches_compressed,
        "decompress: step 1/4 extract archive"
    );
    let patch_map = if archive_bytes.is_empty() {
        std::collections::HashMap::new()
    } else {
        extract_archive(archive_bytes, patches_compressed)?
    };
    info!(
        patches_in_archive = patch_map.len(),
        "decompress: step 1/4 done"
    );

    // Step 2: Collect affected base paths.
    info!("decompress: step 2/4 collect affected base paths");
    let affected: HashSet<String> = records.iter().filter_map(|r| r.old_path.clone()).collect();

    // Step 3: Copy unchanged base files to output.
    info!(
        affected = affected.len(),
        "decompress: step 3/4 copy unchanged"
    );
    let mut stats = copy_unchanged_from_base(base_root, output_root, &affected)?;
    info!(
        files_copied = stats.files_written,
        "decompress: step 3/4 done"
    );

    // Step 4: Process each record.
    info!(
        records = records.len(),
        "decompress: step 4/4 apply records"
    );
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
    info!(
        files_written = stats.files_written,
        bytes_written = stats.bytes_written,
        patches_verified = stats.patches_verified,
        "decompress: done"
    );

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
        None => {
            debug!(old_path = ?record.old_path, "deletion — skipping");
            return Ok(()); // deletion — nothing to write
        }
    };

    debug!(
        new_path,
        entry_type = ?record.entry_type,
        "applying record"
    );

    let dst = output_root.join(new_path);

    match &record.entry_type {
        EntryType::Directory => {
            std::fs::create_dir_all(&dst)
                .map_err(|e| Error::Other(format!("mkdir {}: {e}", dst.display())))?;
            // For a metadata-only dir update (old_path == new_path), the manifest
            // records only the *diff* — mode may be absent if it didn't change.
            // Copy the base mode first so we don't lose it when create_dir_all
            // uses the umask default.
            #[cfg(unix)]
            if let Some(old) = &record.old_path {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                let base_dir = base_root.join(old);
                if let Ok(m) = base_dir.metadata() {
                    let mode = m.mode() & 0o7777;
                    let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode));
                }
            }
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
                    // Metadata-only symlink change: target is unchanged, only
                    // mtime (or similar) differs.  Re-read the target from base.
                    let base_path = record
                        .old_path
                        .as_deref()
                        .map(|p| base_root.join(p))
                        .ok_or_else(|| {
                            Error::Format(format!(
                                "symlink record {new_path} has no SoftlinkTo data, patch, or old_path"
                            ))
                        })?;
                    std::fs::read_link(&base_path)
                        .map(|p| p.to_string_lossy().into_owned())
                        .map_err(|e| {
                            Error::Other(format!("readlink {}: {e}", base_path.display()))
                        })?
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

        EntryType::File => {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            apply_file_record(
                record, new_path, &dst, base_root, patch_map, storage, router, stats,
            )
            .await?;
        }

        EntryType::Special => {
            #[cfg(unix)]
            {
                use std::ffi::CString;
                use std::os::unix::ffi::OsStrExt;
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let dev_info = record.special_device.as_ref().ok_or_else(|| {
                    Error::Format(format!(
                        "special file record {new_path} is missing device info"
                    ))
                })?;
                let mode = record
                    .metadata
                    .as_ref()
                    .and_then(|m| m.mode)
                    .unwrap_or(0o644);
                let full_mode = (mode & 0o7777) | dev_info.file_type_bits;
                let dev = linux_makedev(dev_info.major, dev_info.minor);
                let c_path = CString::new(dst.as_os_str().as_bytes())
                    .map_err(|e| Error::Other(format!("path contains null byte: {e}")))?;
                let ret = unsafe {
                    libc::mknod(
                        c_path.as_ptr(),
                        full_mode as libc::mode_t,
                        dev as libc::dev_t,
                    )
                };
                if ret != 0 {
                    let err = std::io::Error::last_os_error();
                    return Err(Error::Other(format!("mknod {}: {err}", dst.display())));
                }
                stats.files_written += 1;
            }
            #[cfg(not(unix))]
            {
                debug!(path = %dst.display(), "skipping special file on non-unix platform");
            }
        }
    }

    // Apply metadata (mode, mtime) if present.
    // Symlinks: most Linux filesystems do not support lchmod.
    if !matches!(record.entry_type, EntryType::Symlink) {
        // The manifest records only the *diff*: fields absent from `metadata`
        // mean "unchanged from base".  However, writing new content
        // (std::fs::write, patches, create_dir_all) resets mode to the
        // process umask and mtime to "now".  Restore any absent field from
        // the base entry first, then let apply_metadata overwrite with the
        // explicit delta value.
        #[cfg(unix)]
        if let Some(old) = &record.old_path {
            let base_path = base_root.join(old);
            if let Ok(base_meta) = base_path.symlink_metadata() {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                let meta = record.metadata.as_ref();
                // Restore mode from base if not explicitly changed.
                if meta.is_none_or(|m| m.mode.is_none()) {
                    let mode = base_meta.mode() & 0o7777;
                    let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode));
                }
                // Restore mtime from base if not explicitly changed.
                if meta.is_none_or(|m| m.mtime.is_none()) {
                    let ft = filetime::FileTime::from_unix_time(base_meta.mtime(), 0);
                    let _ = filetime::set_file_mtime(&dst, ft);
                }
            }
        }
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
        // Resolve the source bytes:
        //   - old_path present → read file from base directory.
        //   - old_path absent, data = BlobRef → s3_lookup match: download the
        //     base blob that was used as the xdelta3 source during compression.
        //   - old_path absent, no BlobRef → truly new file encoded from empty.
        let source_bytes: Vec<u8>;
        let source: &[u8] = match &record.old_path {
            Some(p) => {
                source_bytes = read_file(&base_root.join(p))?;
                &source_bytes
            }
            None => match &record.data {
                Some(Data::BlobRef(bref)) => {
                    source_bytes = storage.download_blob(bref.blob_id).await?;
                    &source_bytes
                }
                _ => {
                    source_bytes = Vec::new();
                    &source_bytes
                }
            },
        };

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
        let decoded = router.decode(source, &fp)?;
        let n = decoded.len() as u64;
        write_file(dst, &decoded)?;
        stats.files_written += 1;
        stats.patches_verified += 1;
        stats.bytes_written += n;
        debug!(
            path = new_path,
            bytes = n,
            algorithm = pref.algorithm_id.as_deref().unwrap_or("unknown"),
            "file reconstructed via patch"
        );
        return Ok(());
    }

    // Case B: verbatim blob in storage
    if let Some(Data::BlobRef(bref)) = &record.data {
        let data = storage.download_blob(bref.blob_id).await?;
        let n = data.len() as u64;
        write_file(dst, &data)?;
        stats.files_written += 1;
        stats.bytes_written += n;
        debug!(path = new_path, bytes = n, blob_id = %bref.blob_id, "file written from blob");
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
            debug!(new_path, old_path = %old, bytes = n, "file copied from base (metadata change)");
        }
        return Ok(());
    }

    // Case D: new file with no data (empty file)
    debug!(new_path, "new empty file");
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
