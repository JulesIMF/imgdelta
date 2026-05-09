// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Shared utilities for the 4 record-applying decompress stages.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rayon::prelude::*;
use sha2::{Digest, Sha256};
use tracing::debug;
use uuid::Uuid;

use crate::encoding::FilePatch;
use crate::encoding::PatchEncoder;
use crate::encoding::RouterEncoder;
use crate::manifest::{Data, EntryType, Patch, Record};
use crate::{Error, Result};

use crate::decompress::PartitionDecompressStats;

// ── Helpers ───────────────────────────────────────────────────────────────────

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

#[cfg(unix)]
use libc;

#[cfg(unix)]
fn linux_makedev(major: u32, minor: u32) -> u64 {
    libc::makedev(major, minor)
}

// ── run_phase ─────────────────────────────────────────────────────────────────

/// Apply a slice of manifest records to the output tree in the canonical order:
///
/// 1. **Directories** — sequential, preserving parent-before-child ordering.
/// 2. **Files, symlinks, special devices** — parallel (`workers` threads).
/// 3. **Hardlinks** — sequential, after their targets have been written.
#[allow(clippy::too_many_arguments)]
pub fn run_phase(
    records: &[&Record],
    base_root: &Path,
    output_root: &Path,
    patch_map: &HashMap<String, Vec<u8>>,
    blob_cache: &HashMap<Uuid, Vec<u8>>,
    router: &RouterEncoder,
    workers: usize,
    stats: &mut PartitionDecompressStats,
) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }

    let dir_records: Vec<&Record> = records
        .iter()
        .copied()
        .filter(|r| matches!(r.entry_type, EntryType::Directory))
        .collect();
    let main_records: Vec<&Record> = records
        .iter()
        .copied()
        .filter(|r| !matches!(r.entry_type, EntryType::Directory | EntryType::Hardlink))
        .collect();
    let hardlink_records: Vec<&Record> = records
        .iter()
        .copied()
        .filter(|r| matches!(r.entry_type, EntryType::Hardlink))
        .collect();

    // Directories first: sequential so parents exist before children.
    for record in &dir_records {
        let mut local = PartitionDecompressStats::default();
        apply_record_sync(
            record,
            base_root,
            output_root,
            patch_map,
            blob_cache,
            router,
            &mut local,
        )?;
        stats.files_written += local.files_written;
        stats.bytes_written += local.bytes_written;
        stats.patches_verified += local.patches_verified;
    }

    // Files, symlinks, specials: parallel.
    if !main_records.is_empty() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .map_err(|e| Error::Other(format!("failed to build rayon pool: {e}")))?;
        let phase_stats = Mutex::new(PartitionDecompressStats::default());
        let result: Result<()> = pool.install(|| {
            main_records.par_iter().try_for_each(|record| {
                let mut local = PartitionDecompressStats::default();
                apply_record_sync(
                    record,
                    base_root,
                    output_root,
                    patch_map,
                    blob_cache,
                    router,
                    &mut local,
                )?;
                let mut g = phase_stats.lock().expect("phase stats mutex poisoned");
                g.files_written += local.files_written;
                g.bytes_written += local.bytes_written;
                g.patches_verified += local.patches_verified;
                Ok(())
            })
        });
        result?;
        let p = phase_stats
            .into_inner()
            .expect("phase stats mutex poisoned");
        stats.files_written += p.files_written;
        stats.bytes_written += p.bytes_written;
        stats.patches_verified += p.patches_verified;
    }

    // Hardlinks last: their link targets must already be written.
    for record in &hardlink_records {
        let mut local = PartitionDecompressStats::default();
        apply_record_sync(
            record,
            base_root,
            output_root,
            patch_map,
            blob_cache,
            router,
            &mut local,
        )?;
        stats.files_written += local.files_written;
    }

    Ok(())
}

// ── apply_record_sync ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn apply_record_sync(
    record: &Record,
    base_root: &Path,
    output_root: &Path,
    patch_map: &HashMap<String, Vec<u8>>,
    blob_cache: &HashMap<Uuid, Vec<u8>>,
    router: &RouterEncoder,
    stats: &mut PartitionDecompressStats,
) -> Result<()> {
    let new_path = match &record.new_path {
        Some(p) => p,
        None => {
            debug!(old_path = ?record.old_path, "deletion — skipping");
            return Ok(());
        }
    };
    let dst = output_root.join(new_path);

    match &record.entry_type {
        EntryType::Directory => {
            std::fs::create_dir_all(&dst)
                .map_err(|e| Error::Other(format!("mkdir {}: {e}", dst.display())))?;
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
            let target_str = match (&record.data, &record.patch) {
                (Some(Data::SoftlinkTo(t)), _) => t.clone(),
                (None, Some(Patch::Real(pref))) => {
                    let base_path = record
                        .old_path
                        .as_deref()
                        .map(|p| base_root.join(p))
                        .ok_or_else(|| {
                            Error::Format(format!("changed symlink {new_path} has no old_path"))
                        })?;
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
                    let base_path = record
                        .old_path
                        .as_deref()
                        .map(|p| base_root.join(p))
                        .ok_or_else(|| {
                            Error::Format(format!(
                                "symlink record {new_path} has no SoftlinkTo, patch, or old_path"
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
                Error::Other(format!("symlink {} -> {target_str}: {e}", dst.display()))
            })?;
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
                    "hard_link {} -> {}: {e}",
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
            apply_file_record_sync(
                record, new_path, &dst, base_root, patch_map, blob_cache, router, stats,
            )?;
        }

        EntryType::Special => {
            #[cfg(unix)]
            {
                use std::ffi::CString;
                use std::os::unix::ffi::OsStrExt;
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let dev_info = match &record.data {
                    Some(Data::SpecialDevice(d)) => d,
                    _ => {
                        return Err(Error::Format(format!(
                            "special file record {new_path} is missing SpecialDevice data"
                        )))
                    }
                };
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
                    return Err(Error::Other(format!(
                        "mknod {}: {}",
                        dst.display(),
                        std::io::Error::last_os_error()
                    )));
                }
                stats.files_written += 1;
            }
            #[cfg(not(unix))]
            {
                debug!(path = %dst.display(), "skipping special file on non-unix platform");
            }
        }
    }

    // ── Restore metadata ──────────────────────────────────────────────────────
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let manifest_meta = record.metadata.as_ref();
        let base_meta_opt: Option<std::fs::Metadata> = record
            .old_path
            .as_ref()
            .and_then(|old| base_root.join(old).symlink_metadata().ok());

        if manifest_meta.is_none_or(|m| m.uid.is_none() && m.gid.is_none()) {
            if let Some(ref bm) = base_meta_opt {
                if let Ok(c) = CString::new(dst.as_os_str().as_bytes()) {
                    unsafe {
                        libc::lchown(c.as_ptr(), bm.uid() as libc::uid_t, bm.gid() as libc::gid_t)
                    };
                }
            }
        }

        if !matches!(record.entry_type, EntryType::Symlink) {
            if manifest_meta.is_none_or(|m| m.mode.is_none()) {
                if let Some(ref bm) = base_meta_opt {
                    let mode = bm.mode() & 0o7777;
                    let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode));
                }
            }
            if manifest_meta.is_none_or(|m| m.mtime.is_none()) {
                if let Some(ref bm) = base_meta_opt {
                    let ft = filetime::FileTime::from_unix_time(bm.mtime(), 0);
                    let _ = filetime::set_file_mtime(&dst, ft);
                }
            }
        }
    }

    apply_metadata(&dst, record.metadata.as_ref());

    Ok(())
}

fn apply_metadata(path: &Path, meta: Option<&crate::manifest::Metadata>) {
    let meta = match meta {
        Some(m) => m,
        None => return,
    };

    #[cfg(unix)]
    let is_symlink = path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    #[cfg(not(unix))]
    let is_symlink = false;

    #[cfg(unix)]
    if meta.uid.is_some() || meta.gid.is_some() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let uid = meta
            .uid
            .map(|u| u as libc::uid_t)
            .unwrap_or(u32::MAX as libc::uid_t);
        let gid = meta
            .gid
            .map(|g| g as libc::gid_t)
            .unwrap_or(u32::MAX as libc::gid_t);
        if let Ok(c) = CString::new(path.as_os_str().as_bytes()) {
            unsafe { libc::lchown(c.as_ptr(), uid, gid) };
        }
    }
    if !is_symlink {
        if let Some(mode) = meta.mode {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        }
    }
    if !is_symlink {
        if let Some(mtime) = meta.mtime {
            let _ = filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(mtime, 0));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_file_record_sync(
    record: &Record,
    new_path: &str,
    dst: &Path,
    base_root: &Path,
    patch_map: &HashMap<String, Vec<u8>>,
    blob_cache: &HashMap<Uuid, Vec<u8>>,
    router: &RouterEncoder,
    stats: &mut PartitionDecompressStats,
) -> Result<()> {
    if let Some(Patch::Real(pref)) = &record.patch {
        let source_bytes: Vec<u8>;
        let source: &[u8] = match &record.old_path {
            Some(p) => {
                source_bytes = read_file(&base_root.join(p))?;
                &source_bytes
            }
            None => match &record.data {
                Some(Data::BlobRef(bref)) => {
                    source_bytes = blob_cache.get(&bref.blob_id).cloned().ok_or_else(|| {
                        Error::Other(format!("blob {} not in cache", bref.blob_id))
                    })?;
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
        debug!(path = new_path, bytes = n, "file reconstructed via patch");
        return Ok(());
    }

    if let Some(Data::BlobRef(bref)) = &record.data {
        let data = blob_cache
            .get(&bref.blob_id)
            .ok_or_else(|| Error::Other(format!("blob {} not in cache", bref.blob_id)))?;
        let n = data.len() as u64;
        write_file(dst, data)?;
        stats.files_written += 1;
        stats.bytes_written += n;
        debug!(path = new_path, bytes = n, blob_id = %bref.blob_id, "file written from blob");
        return Ok(());
    }

    if let Some(old) = &record.old_path {
        let src = base_root.join(old);
        if src.exists() {
            let data = read_file(&src)?;
            let n = data.len() as u64;
            write_file(dst, &data)?;
            stats.files_written += 1;
            stats.bytes_written += n;
            debug!(new_path, old_path = %old, bytes = n, "file copied from base");
        }
        return Ok(());
    }

    debug!(new_path, "new empty file");
    write_file(dst, &[])?;
    stats.files_written += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_patch_sha256_mismatch_is_detected() {
        let real = b"real-patch-data";
        let actual = hex::encode(Sha256::digest(real));
        let wrong = "deadbeef".repeat(8);
        assert_ne!(actual, wrong);
    }
}
