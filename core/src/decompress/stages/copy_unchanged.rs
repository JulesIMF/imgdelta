// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress stage 2: copy unchanged base files to output

use std::collections::HashSet;
use std::path::Path;

use async_trait::async_trait;
use tracing::debug;
use walkdir::WalkDir;

use crate::decompress::context::DecompressContext;
use crate::decompress::draft::DecompressDraft;
use crate::decompress::stage::DecompressStage;
use crate::{Error, Result};

use super::super::PartitionDecompressStats;

#[cfg(unix)]
use libc;

/// Call `lchown(2)` on `path` — does not follow symlinks.
#[cfg(unix)]
fn lchown_path(path: &Path, uid: u32, gid: u32) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    if let Ok(c) = CString::new(path.as_os_str().as_bytes()) {
        unsafe { libc::lchown(c.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) };
    }
}

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 2: Copy all base-tree entries whose relative path is NOT listed as
/// `old_path` in the manifest records into `output_root`.
pub struct CopyUnchanged;

#[async_trait]
impl DecompressStage for CopyUnchanged {
    fn name(&self) -> &'static str {
        "copy_unchanged"
    }

    async fn run(
        &self,
        ctx: &DecompressContext,
        mut draft: DecompressDraft,
    ) -> Result<DecompressDraft> {
        let affected: HashSet<String> = ctx
            .records
            .iter()
            .filter_map(|r| r.old_path.clone())
            .collect();
        let copy_stats = copy_unchanged_fn(&ctx.base_root, &ctx.output_root, &affected)?;
        draft.stats.files_written += copy_stats.files_written;
        draft.stats.bytes_written += copy_stats.bytes_written;
        draft.stats.patches_verified += copy_stats.patches_verified;
        Ok(draft)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

#[cfg(unix)]
fn linux_makedev(major: u32, minor: u32) -> u64 {
    libc::makedev(major, minor)
}

pub fn copy_unchanged_fn(
    base_root: &Path,
    output_root: &Path,
    affected: &HashSet<String>,
) -> Result<PartitionDecompressStats> {
    let mut stats = PartitionDecompressStats::default();

    #[cfg(unix)]
    let mut hardlink_map: std::collections::HashMap<(u64, u64), std::path::PathBuf> =
        std::collections::HashMap::new();

    for entry in WalkDir::new(base_root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let abs = entry.path();
        let rel = match abs.strip_prefix(base_root) {
            Ok(r) if !r.as_os_str().is_empty() => r.to_string_lossy().into_owned(),
            _ => continue,
        };
        let rel = rel.replace(std::path::MAIN_SEPARATOR, "/");

        if affected.contains(&rel) {
            continue;
        }

        let dst = output_root.join(&rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let ft = entry.file_type();

        match () {
            _ if ft.is_dir() => {
                std::fs::create_dir_all(&dst)
                    .map_err(|e| Error::Other(format!("create_dir {}: {e}", dst.display())))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    if let Ok(m) = abs.metadata() {
                        // chown first, then chmod (chown clears suid/sgid bits).
                        lchown_path(&dst, m.uid(), m.gid());
                        let mode = m.mode() & 0o7777;
                        let _ =
                            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode));
                    }
                }
            }

            _ if ft.is_symlink() => {
                let link_target = std::fs::read_link(abs)
                    .map_err(|e| Error::Other(format!("readlink {}: {e}", abs.display())))?;
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&link_target, &dst).map_err(|e| {
                        Error::Other(format!(
                            "symlink {} -> {}: {e}",
                            dst.display(),
                            link_target.display()
                        ))
                    })?;
                    // Preserve owner of the symlink itself.
                    use std::os::unix::fs::MetadataExt;
                    if let Ok(m) = abs.symlink_metadata() {
                        lchown_path(&dst, m.uid(), m.gid());
                    }
                }
                stats.files_written += 1;
            }

            _ if ft.is_file() => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if let Ok(meta) = abs.symlink_metadata() {
                        let key = (meta.dev(), meta.ino());
                        if meta.nlink() > 1 {
                            if let Some(first) = hardlink_map.get(&key) {
                                std::fs::hard_link(first, &dst).map_err(|e| {
                                    Error::Other(format!(
                                        "hard_link {} -> {}: {e}",
                                        first.display(),
                                        dst.display()
                                    ))
                                })?;
                                stats.files_written += 1;
                                continue;
                            }
                            hardlink_map.insert(key, dst.clone());
                        }
                    }
                }
                let data = std::fs::read(abs)
                    .map_err(|e| Error::Other(format!("read {}: {e}", abs.display())))?;
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
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    // chown FIRST, then chmod (chown clears suid/sgid bits).
                    if let Some(ref m) = src_meta {
                        lchown_path(&dst, m.uid(), m.gid());
                    }
                    if let Some(mode) = src_mode {
                        let _ =
                            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode));
                    }
                }
                if let Some(ft) = src_mtime {
                    let _ = filetime::set_file_mtime(&dst, ft);
                }
                stats.files_written += 1;
            }

            _ => {
                #[cfg(unix)]
                {
                    use std::ffi::CString;
                    use std::os::unix::ffi::OsStrExt;
                    use std::os::unix::fs::MetadataExt;
                    if let Ok(meta) = abs.symlink_metadata() {
                        let rdev = meta.rdev();
                        let mode = meta.mode();
                        let dev = linux_makedev(libc::major(rdev) as u32, libc::minor(rdev) as u32);
                        if let Ok(c_path) = CString::new(dst.as_os_str().as_bytes()) {
                            let ret = unsafe {
                                libc::mknod(
                                    c_path.as_ptr(),
                                    mode as libc::mode_t,
                                    dev as libc::dev_t,
                                )
                            };
                            if ret == 0 {
                                // chown FIRST (mknod sets mode via the call itself;
                                // chown would clear suid/sgid, so chmod is re-applied).
                                lchown_path(&dst, meta.uid(), meta.gid());
                                // Re-apply mode since chown may have cleared suid/sgid.
                                if let Ok(c2) = CString::new(dst.as_os_str().as_bytes()) {
                                    unsafe { libc::chmod(c2.as_ptr(), mode as libc::mode_t) };
                                }
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
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy_unchanged_skips_affected() {
        let base = tempfile::TempDir::new().unwrap();
        let out = tempfile::TempDir::new().unwrap();
        std::fs::write(base.path().join("keep.txt"), b"keep").unwrap();
        std::fs::write(base.path().join("skip.txt"), b"skip").unwrap();
        let affected: HashSet<String> = ["skip.txt".into()].into();
        copy_unchanged_fn(base.path(), out.path(), &affected).unwrap();
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
        copy_unchanged_fn(base.path(), out.path(), &affected).unwrap();
        assert_eq!(std::fs::read(out.path().join("sub/a.txt")).unwrap(), b"a");
        assert!(!out.path().join("sub/b.txt").exists());
    }
}
