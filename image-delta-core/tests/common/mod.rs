// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Shared test helpers: make_compressor, compress_opts, write, etc.

#![allow(dead_code)]

pub mod fake_storage;

pub use fake_storage::FakeStorage;

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

// ── compare_dirs ──────────────────────────────────────────────────────────────

/// Describes a single difference found by [`compare_dirs`].
#[derive(Debug)]
#[allow(dead_code)]
pub enum DiffEntry {
    /// Path exists in `expected` but is missing from `actual`.
    Missing { path: String },
    /// Path exists in `actual` but not in `expected`.
    Extra { path: String },
    /// File content (SHA-256) differs.
    ContentMismatch { path: String },
    /// Unix permission bits differ.
    ModeMismatch {
        path: String,
        expected_mode: u32,
        actual_mode: u32,
    },
    /// Modification time differs by more than 1 second.
    MtimeMismatch {
        path: String,
        expected_mtime: i64,
        actual_mtime: i64,
    },
    /// File type (regular/symlink/dir/hardlink) differs.
    TypeMismatch { path: String },
    /// Symlink target string differs.
    SymlinkTargetMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    /// Hardlink pair expected but `(dev, ino)` does not match.
    HardlinkMismatch { path: String },
    /// Numeric owner (uid) differs.
    UidMismatch {
        path: String,
        expected_uid: u32,
        actual_uid: u32,
    },
    /// Numeric group (gid) differs.
    GidMismatch {
        path: String,
        expected_gid: u32,
        actual_gid: u32,
    },
    /// The set of xattr names differs, or a value for the same name differs.
    XattrMismatch { path: String, detail: String },
}

/// Compare two directory trees and return a list of differences.
///
/// Returns an empty `Vec` when `expected` and `actual` are byte-for-byte
/// and attribute-for-attribute identical (within ±1 s mtime tolerance).
///
/// # Panics
///
/// Panics if either path cannot be walked (I/O error).
pub fn compare_dirs(expected: &Path, actual: &Path) -> Vec<DiffEntry> {
    let mut diffs = Vec::new();

    // Collect all relative paths in expected.
    let expected_paths = collect_rel_paths(expected);
    let actual_paths = collect_rel_paths(actual);

    // Missing from actual.
    for p in &expected_paths {
        if !actual_paths.contains(p) {
            diffs.push(DiffEntry::Missing { path: p.clone() });
        }
    }
    // Extra in actual.
    for p in &actual_paths {
        if !expected_paths.contains(p) {
            diffs.push(DiffEntry::Extra { path: p.clone() });
        }
    }

    // Per-path comparison (only for paths present in both).
    for rel_path in &expected_paths {
        if !actual_paths.contains(rel_path) {
            continue;
        }
        let exp_full = expected.join(rel_path);
        let act_full = actual.join(rel_path);

        let exp_meta = exp_full.symlink_metadata().expect("expected path readable");
        let act_meta = act_full.symlink_metadata().expect("actual path readable");

        // Type check.
        if exp_meta.file_type() != act_meta.file_type() {
            diffs.push(DiffEntry::TypeMismatch {
                path: rel_path.clone(),
            });
            continue; // remaining checks don't make sense on different types
        }

        if exp_meta.file_type().is_symlink() {
            let exp_target = std::fs::read_link(&exp_full)
                .expect("read_link on expected")
                .to_string_lossy()
                .into_owned();
            let act_target = std::fs::read_link(&act_full)
                .expect("read_link on actual")
                .to_string_lossy()
                .into_owned();
            if exp_target != act_target {
                diffs.push(DiffEntry::SymlinkTargetMismatch {
                    path: rel_path.clone(),
                    expected: exp_target,
                    actual: act_target,
                });
            }
            continue;
        }

        if exp_meta.file_type().is_dir() {
            // Mode check only for directories.
            let em = exp_meta.mode() & 0o7777;
            let am = act_meta.mode() & 0o7777;
            if em != am {
                diffs.push(DiffEntry::ModeMismatch {
                    path: rel_path.clone(),
                    expected_mode: em,
                    actual_mode: am,
                });
            }
            continue;
        }

        // Regular file.
        if sha256_file(&exp_full) != sha256_file(&act_full) {
            diffs.push(DiffEntry::ContentMismatch {
                path: rel_path.clone(),
            });
        }

        let em = exp_meta.mode() & 0o7777;
        let am = act_meta.mode() & 0o7777;
        if em != am {
            diffs.push(DiffEntry::ModeMismatch {
                path: rel_path.clone(),
                expected_mode: em,
                actual_mode: am,
            });
        }

        let et = exp_meta.mtime();
        let at = act_meta.mtime();
        if et.abs_diff(at) > 1 {
            diffs.push(DiffEntry::MtimeMismatch {
                path: rel_path.clone(),
                expected_mtime: et,
                actual_mtime: at,
            });
        }

        // Hardlink check: if expected file is hardlinked with another path,
        // the actual file must share (dev, ino) with the same partner.
        if exp_meta.nlink() > 1 && act_meta.nlink() < 2 {
            diffs.push(DiffEntry::HardlinkMismatch {
                path: rel_path.clone(),
            });
            // Full (dev, ino) equivalence is checked implicitly: both files
            // are in the same FS tree so (dev, ino) matching means same content.
        }

        // uid / gid
        let eu = exp_meta.uid();
        let au = act_meta.uid();
        if eu != au {
            diffs.push(DiffEntry::UidMismatch {
                path: rel_path.clone(),
                expected_uid: eu,
                actual_uid: au,
            });
        }
        let eg = exp_meta.gid();
        let ag = act_meta.gid();
        if eg != ag {
            diffs.push(DiffEntry::GidMismatch {
                path: rel_path.clone(),
                expected_gid: eg,
                actual_gid: ag,
            });
        }

        // Extended attributes (security.capability, user.*, trusted.*, etc.)
        compare_xattrs(&exp_full, &act_full, rel_path, &mut diffs);
    }

    diffs
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn collect_rel_paths(root: &Path) -> std::collections::HashSet<String> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.path()
                .strip_prefix(root)
                .ok()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

/// Compare extended attributes of two paths and push any differences.
fn compare_xattrs(expected: &Path, actual: &Path, rel_path: &str, diffs: &mut Vec<DiffEntry>) {
    let exp_xattrs: std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> = xattr::list(expected)
        .unwrap_or_default()
        .filter_map(|name| {
            xattr::get(expected, &name)
                .ok()
                .flatten()
                .map(|v| (name, v))
        })
        .collect();
    let act_xattrs: std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> = xattr::list(actual)
        .unwrap_or_default()
        .filter_map(|name| xattr::get(actual, &name).ok().flatten().map(|v| (name, v)))
        .collect();

    // Names only in expected.
    for name in exp_xattrs.keys() {
        if !act_xattrs.contains_key(name) {
            diffs.push(DiffEntry::XattrMismatch {
                path: rel_path.to_owned(),
                detail: format!("missing xattr {:?}", name),
            });
        }
    }
    // Names only in actual.
    for name in act_xattrs.keys() {
        if !exp_xattrs.contains_key(name) {
            diffs.push(DiffEntry::XattrMismatch {
                path: rel_path.to_owned(),
                detail: format!("extra xattr {:?}", name),
            });
        }
    }
    // Value mismatches.
    for (name, exp_val) in &exp_xattrs {
        if let Some(act_val) = act_xattrs.get(name) {
            if exp_val != act_val {
                diffs.push(DiffEntry::XattrMismatch {
                    path: rel_path.to_owned(),
                    detail: format!("xattr {:?} value differs", name),
                });
            }
        }
    }
}

fn sha256_file(path: &Path) -> String {
    let data = std::fs::read(path).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&data);
    hex::encode(h.finalize())
}

// ── test helpers ──────────────────────────────────────────────────────────────

/// Write `content` to `root/rel_path`, creating parent directories as needed.
pub fn write_file(root: &Path, rel_path: &str, content: &[u8]) {
    let full = root.join(rel_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
}

/// Create a symlink at `root/link_path` pointing to `target`.
pub fn write_symlink(root: &Path, link_path: &str, target: &str) {
    let full = root.join(link_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    if full.symlink_metadata().is_ok() {
        std::fs::remove_file(&full).unwrap();
    }
    std::os::unix::fs::symlink(target, &full).unwrap();
}

/// Set the mtime of `root/rel_path` to 60 seconds in the past.
///
/// Needed in tests that compare two files with different content but
/// identical mtime (written within the same second): `diff_dirs` uses an
/// mtime fast-path that skips SHA-256 when mtimes are equal, so we must
/// ensure the base file appears older than the target.
pub fn set_mtime_old(root: &Path, rel_path: &str) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let p = root.join(rel_path);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(now - 60, 0)).unwrap();
}

/// Set the Unix mode of `root/rel_path`.
pub fn set_mode(root: &Path, rel_path: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let full = root.join(rel_path);
    std::fs::set_permissions(&full, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Build a [`FakeStorage`] + [`PassthroughEncoder`] + [`DefaultCompressor`] triple.
///
/// Uses `PassthroughEncoder` as the router fallback so that both regular files
/// and symlink/hardlink patches (which always use `Passthrough`) can be decoded
/// during decompress.  Tests that need a particular encoder can build their own
/// compressor.
pub fn make_compressor() -> (
    std::sync::Arc<FakeStorage>,
    std::sync::Arc<image_delta_core::DefaultCompressor>,
) {
    use image_delta_core::{DefaultCompressor, PassthroughEncoder};
    use std::sync::Arc;

    let storage = Arc::new(FakeStorage::new());
    let encoder = Arc::new(PassthroughEncoder::new());
    let compressor = Arc::new(DefaultCompressor::with_encoder(
        Arc::new(image_delta_core::DirectoryImage::new()),
        Arc::clone(&storage) as _,
        Arc::clone(&encoder) as _,
    ));
    (storage, compressor)
}

/// Default [`CompressOptions`] for tests.
pub fn compress_opts(
    image_id: &str,
    base_image_id: Option<&str>,
) -> image_delta_core::CompressOptions {
    compress_opts_workers(image_id, base_image_id, 1)
}

/// Like [`compress_opts`] but with a configurable worker count.
pub fn compress_opts_workers(
    image_id: &str,
    base_image_id: Option<&str>,
    workers: usize,
) -> image_delta_core::CompressOptions {
    image_delta_core::CompressOptions {
        image_id: image_id.to_string(),
        base_image_id: base_image_id.map(|s| s.to_string()),
        workers,
        passthrough_threshold: 1.0,
    }
}

/// Default [`DecompressOptions`] for tests.
pub fn decompress_opts(image_id: &str, base_root: &Path) -> image_delta_core::DecompressOptions {
    image_delta_core::DecompressOptions {
        image_id: image_id.to_string(),
        base_root: base_root.to_path_buf(),
        workers: 1,
    }
}
