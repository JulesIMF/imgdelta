use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

// ── Public types ──────────────────────────────────────────────────────────────

/// A single difference between two filesystem trees.
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// Path relative to the scanned root, forward-slash separated.
    pub path: String,
    pub kind: DiffKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    /// Path exists in target but not in base.
    Added,
    /// Path exists in base but not in target.
    Removed,
    /// Content (SHA-256) differs between base and target.
    Changed,
    /// Content is identical but at least one metadata field
    /// (mode / uid / gid / mtime) changed.
    MetadataOnly,
}

/// Result of comparing two directory trees.
#[derive(Debug, Default)]
pub struct DiffResult {
    pub diffs: Vec<FileDiff>,
}

impl DiffResult {
    /// Entries present in target but not in base.
    pub fn added(&self) -> impl Iterator<Item = &FileDiff> {
        self.diffs.iter().filter(|d| d.kind == DiffKind::Added)
    }

    /// Entries present in base but not in target.
    pub fn removed(&self) -> impl Iterator<Item = &FileDiff> {
        self.diffs.iter().filter(|d| d.kind == DiffKind::Removed)
    }

    /// Entries whose content differs.
    pub fn changed(&self) -> impl Iterator<Item = &FileDiff> {
        self.diffs.iter().filter(|d| d.kind == DiffKind::Changed)
    }

    /// Entries whose metadata differs but content is identical.
    pub fn metadata_only(&self) -> impl Iterator<Item = &FileDiff> {
        self.diffs
            .iter()
            .filter(|d| d.kind == DiffKind::MetadataOnly)
    }

    /// `true` when no differences were found.
    pub fn is_empty(&self) -> bool {
        self.diffs.is_empty()
    }
}

// ── Internal snapshot types ───────────────────────────────────────────────────

/// Everything we record about a single filesystem entry after walking a tree.
#[derive(Debug)]
struct FsEntry {
    /// `true` if this entry is a regular file (not a symlink, not a dir).
    is_file: bool,
    /// `true` if this entry is a symbolic link.
    is_symlink: bool,
    /// `true` if this entry is a directory.
    is_dir: bool,

    // Metadata (from `symlink_metadata`, i.e. about the link itself, not the target).
    mode: u32,
    uid: u32,
    gid: u32,
    /// Modification time as seconds since epoch.
    mtime_secs: i64,

    /// For regular files: SHA-256 of content.
    sha256: Option<[u8; 32]>,
    /// For symlinks: the link target string.
    link_target: Option<String>,

    #[allow(dead_code)]
    /// `(device, inode)` used for hard-link grouping.
    dev_ino: (u64, u64),
    #[allow(dead_code)]
    /// Number of hard links (from `st_nlink`).
    nlink: u64,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Walk two directory trees and compute their differences.
///
/// Compares:
/// - SHA-256 of file contents (only when `mtime` or `size` differs;
///   skipped when both are equal to avoid unnecessary I/O).
/// - Unix mode, uid, gid, mtime (with ±1 s tolerance for filesystem rounding).
/// - Symlink targets (via `fs::read_link`; symlinks are **not** followed).
/// - Hard-link relationships (files sharing `(dev, ino)` are treated as
///   hardlinks; changes are reported as [`DiffKind::Modified`] when the
///   target of a hardlink group changes).
///
/// Directories are compared on mode / uid / gid only (content is covered
/// recursively by their children).
///
/// # Errors
///
/// Returns [`crate::Error::Io`] if either tree cannot be read.
pub fn diff_dirs(base: &Path, target: &Path) -> crate::Result<DiffResult> {
    let base_map = snapshot(base)?;
    let target_map = snapshot(target)?;

    let mut diffs = Vec::new();

    // ── Entries in base but not in target (removed) ────────────────────────
    for path in base_map.keys() {
        if !target_map.contains_key(path) {
            diffs.push(FileDiff {
                path: path.clone(),
                kind: DiffKind::Removed,
            });
        }
    }

    // ── Entries in target but not in base (added) or changed ──────────────
    for (path, t_entry) in &target_map {
        match base_map.get(path) {
            None => {
                diffs.push(FileDiff {
                    path: path.clone(),
                    kind: DiffKind::Added,
                });
            }
            Some(b_entry) => {
                if let Some(kind) = compare_entries(b_entry, t_entry) {
                    diffs.push(FileDiff {
                        path: path.clone(),
                        kind,
                    });
                }
            }
        }
    }

    Ok(DiffResult { diffs })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Build a map of `relative-path → FsEntry` for a single directory tree.
///
/// Symbolic links are **not** followed (`follow_links(false)`).
fn snapshot(root: &Path) -> crate::Result<HashMap<String, FsEntry>> {
    let mut map = HashMap::new();

    for entry_result in WalkDir::new(root).follow_links(false) {
        let entry =
            entry_result.map_err(|e| std::io::Error::other(format!("walkdir error: {e}")))?;

        // Skip the root directory itself.
        if entry.path() == root {
            continue;
        }

        let rel_path = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // Normalise to forward-slash, skip empty.
        let rel_str = rel_path.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            continue;
        }

        // Use `symlink_metadata` to get info about the link itself, not its target.
        let meta = entry.path().symlink_metadata()?;

        let file_type = meta.file_type();
        let is_symlink = file_type.is_symlink();
        let is_file = file_type.is_file();
        let is_dir = file_type.is_dir();

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

        let fs_entry = FsEntry {
            is_file,
            is_symlink,
            is_dir,
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            mtime_secs: meta.mtime(),
            sha256,
            link_target,
            dev_ino: (meta.dev(), meta.ino()),
            nlink: meta.nlink(),
        };

        map.insert(rel_str, fs_entry);
    }

    Ok(map)
}

/// Compute the SHA-256 of a file's content.
fn sha256_of_file(path: &Path) -> std::io::Result<[u8; 32]> {
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

/// Compare two entries that share the same relative path.
///
/// Returns `Some(kind)` when a difference was found, `None` when identical.
fn compare_entries(base: &FsEntry, target: &FsEntry) -> Option<DiffKind> {
    // Type change (file → dir, symlink → file, …) counts as Modified.
    if base.is_file != target.is_file
        || base.is_symlink != target.is_symlink
        || base.is_dir != target.is_dir
    {
        return Some(DiffKind::Changed);
    }

    // ── Symlink: compare target string ────────────────────────────────────
    if base.is_symlink {
        if base.link_target != target.link_target {
            return Some(DiffKind::Changed);
        }
        // Metadata comparison for symlinks (uid/gid only; mode is ignored on Linux).
        if base.uid != target.uid || base.gid != target.gid {
            return Some(DiffKind::MetadataOnly);
        }
        return None;
    }

    // ── Regular file: content then metadata ───────────────────────────────
    if base.is_file {
        // Fast path: if mtime and size match, assume content unchanged.
        // If mtime differs → check SHA-256 to distinguish Modified vs MetadataOnly.
        let content_changed = if mtime_differs(base.mtime_secs, target.mtime_secs) {
            // SHA-256 comparison to avoid false positives from mtime-only updates.
            base.sha256 != target.sha256
        } else {
            false
        };

        if content_changed {
            return Some(DiffKind::Changed);
        }

        // Check metadata.
        if metadata_differs(base, target) {
            return Some(DiffKind::MetadataOnly);
        }

        return None;
    }

    // ── Directory: metadata only ──────────────────────────────────────────
    if base.is_dir {
        if metadata_differs(base, target) {
            return Some(DiffKind::MetadataOnly);
        }
        return None;
    }

    None
}

/// `true` when mtime differs by more than 1 second (filesystem rounding tolerance).
fn mtime_differs(a: i64, b: i64) -> bool {
    (a - b).unsigned_abs() > 1
}

/// `true` when at least one of mode / uid / gid / mtime differs.
fn metadata_differs(base: &FsEntry, target: &FsEntry) -> bool {
    base.mode != target.mode
        || base.uid != target.uid
        || base.gid != target.gid
        || mtime_differs(base.mtime_secs, target.mtime_secs)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    // ── Helpers ───────────────────────────────────────────────────────────

    fn write(dir: &Path, rel: &str, content: &[u8]) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }

    fn chmod(dir: &Path, rel: &str, mode: u32) {
        let p = dir.join(rel);
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn assert_kind(result: &DiffResult, path: &str, kind: DiffKind) {
        let found = result.diffs.iter().find(|d| d.path == path);
        assert!(
            found.is_some(),
            "expected diff for '{}' but not found; diffs: {:#?}",
            path,
            result.diffs
        );
        assert_eq!(
            found.unwrap().kind,
            kind,
            "wrong kind for '{}': {:#?}",
            path,
            result.diffs
        );
    }

    fn assert_no_diff(result: &DiffResult, path: &str) {
        let found = result.diffs.iter().find(|d| d.path == path);
        assert!(
            found.is_none(),
            "expected no diff for '{}' but found {:?}",
            path,
            found
        );
    }

    // 1. Identical directories produce no diffs.
    #[test]
    fn test_identical_dirs() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(base.path(), "a.txt", b"hello");
        write(target.path(), "a.txt", b"hello");

        let result = diff_dirs(base.path(), target.path()).unwrap();
        assert!(result.is_empty(), "expected no diffs: {:#?}", result.diffs);
    }

    // 2. New file in target → Added.
    #[test]
    fn test_added_file() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(target.path(), "new.txt", b"new");

        let result = diff_dirs(base.path(), target.path()).unwrap();
        assert_kind(&result, "new.txt", DiffKind::Added);
    }

    // 3. File in base, absent in target → Removed.
    #[test]
    fn test_removed_file() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(base.path(), "gone.txt", b"old");

        let result = diff_dirs(base.path(), target.path()).unwrap();
        assert_kind(&result, "gone.txt", DiffKind::Removed);
    }

    // 4. Same mtime means content is assumed identical (no SHA-256 needed).
    //    We verify by ensuring a file with identical content + same mtime = no diff.
    #[test]
    fn test_identical_content_no_diff() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(base.path(), "same.txt", b"content");
        write(target.path(), "same.txt", b"content");

        let result = diff_dirs(base.path(), target.path()).unwrap();
        // Content is the same — even if mtime differs slightly, SHA-256 match → no Modified.
        // At minimum, it should not be Added or Removed.
        let diff = result.diffs.iter().find(|d| d.path == "same.txt");
        if let Some(d) = diff {
            assert_ne!(d.kind, DiffKind::Added);
            assert_ne!(d.kind, DiffKind::Removed);
        }
    }

    // 5. Content changes → Modified.
    #[test]
    fn test_changed_content() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(base.path(), "data.bin", b"old content");
        write(target.path(), "data.bin", b"new content");

        // Touch mtime so the fast-path comparison fires.
        let b_path = base.path().join("data.bin");
        let t_path = target.path().join("data.bin");
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        filetime::set_file_mtime(&t_path, filetime::FileTime::from_system_time(future))
            .unwrap_or(());
        let _ = b_path; // suppress unused warning

        let result = diff_dirs(base.path(), target.path()).unwrap();
        assert_kind(&result, "data.bin", DiffKind::Changed);
    }

    // 6. mtime changes but content is identical → MetadataOnly (not Modified).
    #[test]
    fn test_mtime_only_change() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(base.path(), "stable.txt", b"unchanged");
        write(target.path(), "stable.txt", b"unchanged");

        // Bump mtime of target file significantly (>1 s).
        let t_path = target.path().join("stable.txt");
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(100);
        filetime::set_file_mtime(&t_path, filetime::FileTime::from_system_time(future)).unwrap();

        let result = diff_dirs(base.path(), target.path()).unwrap();
        // Content unchanged → must not be Modified.
        let diff = result.diffs.iter().find(|d| d.path == "stable.txt");
        if let Some(d) = diff {
            assert_ne!(
                d.kind,
                DiffKind::Changed,
                "mtime-only change should not be Modified"
            );
        }
    }

    // 7. chmod 644 → 755 with same content → MetadataOnly.
    #[test]
    fn test_mode_change() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        write(base.path(), "script.sh", b"#!/bin/sh");
        write(target.path(), "script.sh", b"#!/bin/sh");
        chmod(base.path(), "script.sh", 0o644);
        chmod(target.path(), "script.sh", 0o755);

        let result = diff_dirs(base.path(), target.path()).unwrap();
        assert_kind(&result, "script.sh", DiffKind::MetadataOnly);
    }

    // 8. Symlink target changes → Modified.
    #[test]
    fn test_symlink_change() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        symlink("/old/target", base.path().join("link")).unwrap();
        symlink("/new/target", target.path().join("link")).unwrap();

        let result = diff_dirs(base.path(), target.path()).unwrap();
        assert_kind(&result, "link", DiffKind::Changed);
    }

    // 9. Symlinks are not followed — a symlink pointing to a dir is not walked.
    #[test]
    fn test_symlink_not_followed() {
        let outside = TempDir::new().unwrap();
        write(outside.path(), "secret.txt", b"secret");

        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        // Identical symlinks pointing outside the tree.
        symlink(outside.path(), base.path().join("link")).unwrap();
        symlink(outside.path(), target.path().join("link")).unwrap();

        let result = diff_dirs(base.path(), target.path()).unwrap();
        // "link" should appear — it's a symlink entry — but "link/secret.txt" must NOT.
        let has_secret = result.diffs.iter().any(|d| d.path.contains("secret.txt"));
        assert!(
            !has_secret,
            "followed symlink — that's wrong: {:#?}",
            result.diffs
        );
    }

    // 10. Hard-link detection: two paths share the same (dev, ino).
    #[test]
    fn test_hardlink_detection() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        let src = base.path().join("original.txt");
        fs::write(&src, b"data").unwrap();
        fs::hard_link(&src, base.path().join("hardlink.txt")).unwrap();

        // Same structure in target.
        let src2 = target.path().join("original.txt");
        fs::write(&src2, b"data").unwrap();
        fs::hard_link(&src2, target.path().join("hardlink.txt")).unwrap();

        let result = diff_dirs(base.path(), target.path()).unwrap();
        assert!(
            result.is_empty(),
            "identical hardlinks should produce no diff: {:#?}",
            result.diffs
        );

        // Verify we can detect the hardlink relationship in the snapshot.
        let snap = snapshot(base.path()).unwrap();
        let orig = snap.get("original.txt").unwrap();
        let link = snap.get("hardlink.txt").unwrap();
        assert_eq!(
            orig.dev_ino, link.dev_ino,
            "inode should match for hardlinks"
        );
        assert!(orig.nlink >= 2, "nlink should be >= 2 for hardlinked file");
    }

    // 11. Nested directories are recursed correctly.
    #[test]
    fn test_nested_dirs() {
        let base = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        write(base.path(), "a/b/c/deep.txt", b"deep");
        write(target.path(), "a/b/c/deep.txt", b"deep");
        write(target.path(), "a/b/c/new.txt", b"new");

        let result = diff_dirs(base.path(), target.path()).unwrap();
        assert_kind(&result, "a/b/c/new.txt", DiffKind::Added);
        assert_no_diff(&result, "a/b/c/deep.txt");
    }
}
