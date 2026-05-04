// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// In-memory filesystem snapshot: FsTree, FsEntry, EntryMeta

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use filetime::FileTime;
use walkdir::WalkDir;

// ── EntryMeta ─────────────────────────────────────────────────────────────────

/// Per-entry metadata that mirrors what the compressor tracks.
///
/// When writing to disk with [`FsTree::write_to_dir`], `uid` and `gid` are
/// only honoured if the process is running as root; otherwise the current
/// process uid/gid is used.  `xattrs` are never set (portable and safe for
/// non-root test environments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryMeta {
    /// Unix permission bits (e.g. `0o644`, `0o755`).
    pub mode: u32,
    /// Owner user ID.
    pub uid: u32,
    /// Owner group ID.
    pub gid: u32,
    /// Modification time as Unix seconds (may be negative for pre-epoch).
    pub mtime_secs: i64,
}

impl EntryMeta {
    /// Returns a meta that uses the calling process's uid/gid.
    pub fn new(mode: u32, mtime_secs: i64) -> Self {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        Self {
            mode,
            uid,
            gid,
            mtime_secs,
        }
    }
}

// ── FsEntry ───────────────────────────────────────────────────────────────────

/// A single node in the synthetic filesystem tree.
#[derive(Debug, Clone)]
pub enum FsEntry {
    /// Regular file with arbitrary byte content.
    File { content: Vec<u8>, meta: EntryMeta },
    /// Directory (content is implied by the path hierarchy).
    Dir { meta: EntryMeta },
    /// Symbolic link; `target` is the raw string stored in the link.
    Symlink { target: String, meta: EntryMeta },
    /// Hard link; `canonical` is the path of the "real" file in this tree.
    Hardlink { canonical: String, meta: EntryMeta },
}

impl FsEntry {
    /// Returns the metadata for any entry kind.
    pub fn meta(&self) -> &EntryMeta {
        match self {
            FsEntry::File { meta, .. }
            | FsEntry::Dir { meta }
            | FsEntry::Symlink { meta, .. }
            | FsEntry::Hardlink { meta, .. } => meta,
        }
    }

    /// Mutable reference to the metadata.
    pub fn meta_mut(&mut self) -> &mut EntryMeta {
        match self {
            FsEntry::File { meta, .. }
            | FsEntry::Dir { meta }
            | FsEntry::Symlink { meta, .. }
            | FsEntry::Hardlink { meta, .. } => meta,
        }
    }

    /// Returns `true` if this is a `File` entry.
    pub fn is_file(&self) -> bool {
        matches!(self, FsEntry::File { .. })
    }

    /// Returns `true` if this is a `Dir` entry.
    pub fn is_dir(&self) -> bool {
        matches!(self, FsEntry::Dir { .. })
    }

    /// Returns `true` if this is a `Symlink` entry.
    pub fn is_symlink(&self) -> bool {
        matches!(self, FsEntry::Symlink { .. })
    }

    /// Returns `true` if this is a `Hardlink` entry.
    pub fn is_hardlink(&self) -> bool {
        matches!(self, FsEntry::Hardlink { .. })
    }
}

// ── FsTree ────────────────────────────────────────────────────────────────────

/// An in-memory snapshot of a filesystem tree.
///
/// Keys are slash-separated paths relative to the tree root (no leading `/`).
/// Directory entries are stored explicitly; their existence is not inferred
/// from file paths.
///
/// # Invariants (maintained by [`crate::builder::FsTreeBuilder`] and
/// [`crate::mutator::FsMutator`])
///
/// * Every `Hardlink` has a `canonical` key that resolves to a `File` in
///   this tree.
/// * Every `Symlink` may point anywhere (symlinks are not validated).
/// * Every file's parent directory is also present in the tree.
#[derive(Debug, Clone, Default)]
pub struct FsTree {
    pub entries: HashMap<String, FsEntry>,
}

impl FsTree {
    /// Creates an empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns an iterator over `(path, entry)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &FsEntry)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Returns the number of entries in the tree.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the tree has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the entry at `path`, or `None`.
    pub fn get(&self, path: &str) -> Option<&FsEntry> {
        self.entries.get(path)
    }

    /// Insert or replace an entry.
    pub fn insert(&mut self, path: String, entry: FsEntry) {
        self.entries.insert(path, entry);
    }

    /// Remove an entry, returning it if it existed.
    pub fn remove(&mut self, path: &str) -> Option<FsEntry> {
        self.entries.remove(path)
    }

    /// Returns all paths that have an entry (sorted for determinism).
    pub fn paths(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.entries.keys().map(|s| s.as_str()).collect();
        v.sort_unstable();
        v
    }

    /// Returns all file paths (sorted).
    pub fn file_paths(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_file())
            .map(|(k, _)| k.as_str())
            .collect();
        v.sort_unstable();
        v
    }

    /// Returns all directory paths (sorted).
    pub fn dir_paths(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_dir())
            .map(|(k, _)| k.as_str())
            .collect();
        v.sort_unstable();
        v
    }

    /// Returns all symlink paths (sorted).
    pub fn symlink_paths(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_symlink())
            .map(|(k, _)| k.as_str())
            .collect();
        v.sort_unstable();
        v
    }

    /// Returns all hardlink paths (sorted).
    pub fn hardlink_paths(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_hardlink())
            .map(|(k, _)| k.as_str())
            .collect();
        v.sort_unstable();
        v
    }

    /// Materialise the in-memory tree to disk at `root`.
    ///
    /// The directory at `root` must already exist.  All parents are created
    /// as needed.  For each entry:
    ///
    /// * **Dir**: `create_dir_all` + set mode + set mtime.
    /// * **File**: create file + set mode + set mtime.
    /// * **Symlink**: `std::os::unix::fs::symlink`; mode/mtime on the link
    ///   itself is set via `lchown`-style calls (best-effort).
    /// * **Hardlink**: `std::fs::hard_link(canonical_abs, link_abs)`.
    ///
    /// uid/gid are only set when running as root; otherwise they are silently
    /// skipped.  xattrs are never set.
    pub fn write_to_dir(&self, root: &Path) -> std::io::Result<()> {
        let is_root = unsafe { libc::getuid() } == 0;

        // Sort so that parent dirs are created before their children.
        let mut paths: Vec<&str> = self.entries.keys().map(|s| s.as_str()).collect();
        paths.sort_unstable();

        // First pass: dirs, files, symlinks (not hardlinks).
        for path in &paths {
            let entry = &self.entries[*path];
            let abs = root.join(path);

            match entry {
                FsEntry::Dir { meta: _ } => {
                    // Mode and mtime are applied in the third pass so that
                    // restrictive modes (e.g. 0o444) don't block child
                    // creation and mtimes aren't clobbered by later writes.
                    std::fs::create_dir_all(&abs)?;
                }
                FsEntry::File { content, meta } => {
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&abs, content)?;
                    apply_meta(&abs, meta, is_root, false)?;
                }
                FsEntry::Symlink { target, meta } => {
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    // Remove existing entry at this path before creating symlink.
                    let _ = std::fs::remove_file(&abs);
                    std::os::unix::fs::symlink(target, &abs)?;
                    apply_meta(&abs, meta, is_root, true)?;
                }
                FsEntry::Hardlink { .. } => {
                    // Handled in second pass so canonical always exists.
                }
            }
        }

        // Second pass: hardlinks (canonical file must already be on disk).
        for path in &paths {
            let entry = &self.entries[*path];
            if let FsEntry::Hardlink { canonical, .. } = entry {
                let abs = root.join(path);
                let canonical_abs = root.join(canonical);
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::hard_link(&canonical_abs, &abs)?;
            }
        }

        // Third pass: set dir mtimes (they get reset when children are created).
        let mut dir_paths: Vec<&str> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_dir())
            .map(|(k, _)| k.as_str())
            .collect();
        // Third pass: set dir modes and mtimes (deepest first so parent
        // mtimes aren't clobbered by child dir creation, and restrictive
        // modes don't block child operations above).
        dir_paths.sort_unstable_by(|a, b| b.cmp(a));
        for path in dir_paths {
            let abs = root.join(path);
            let meta = self.entries[path].meta();
            apply_meta(&abs, meta, is_root, false)?;
        }

        Ok(())
    }

    /// Read an on-disk directory tree into an `FsTree`.
    ///
    /// Hard links are detected via `(dev, ino)` pairs; the first path seen
    /// (lexicographic order) becomes the `File` entry and subsequent paths
    /// with the same inode become `Hardlink { canonical }`.
    ///
    /// xattrs are ignored.
    pub fn from_dir(root: &Path) -> std::io::Result<Self> {
        let mut tree = FsTree::new();
        let mut inode_map: HashMap<(u64, u64), String> = HashMap::new();

        // Sorted walk so hardlink canonical is always the first path.
        let mut entries: Vec<_> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .strip_prefix(root)
                    .map(|p| !p.as_os_str().is_empty())
                    .unwrap_or(false)
            })
            .collect();
        entries.sort_unstable_by(|a, b| a.path().cmp(b.path()));

        for entry in &entries {
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");

            let raw_meta = entry.path().symlink_metadata()?;
            let meta = EntryMeta {
                mode: raw_meta.mode() & 0o7777,
                uid: raw_meta.uid(),
                gid: raw_meta.gid(),
                mtime_secs: raw_meta.mtime(),
            };

            if raw_meta.file_type().is_symlink() {
                let target = std::fs::read_link(entry.path())?
                    .to_string_lossy()
                    .into_owned();
                tree.insert(rel, FsEntry::Symlink { target, meta });
            } else if raw_meta.file_type().is_dir() {
                tree.insert(rel, FsEntry::Dir { meta });
            } else if raw_meta.file_type().is_file() {
                let key = (raw_meta.dev(), raw_meta.ino());
                if let Some(canonical) = inode_map.get(&key).cloned() {
                    tree.insert(rel, FsEntry::Hardlink { canonical, meta });
                } else {
                    inode_map.insert(key, rel.clone());
                    let content = std::fs::read(entry.path())?;
                    tree.insert(rel, FsEntry::File { content, meta });
                }
            }
        }

        Ok(tree)
    }
}

// ── apply_meta helper ─────────────────────────────────────────────────────────

fn apply_meta(
    path: &Path,
    meta: &EntryMeta,
    is_root: bool,
    is_symlink: bool,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Mode (not meaningful on symlinks — skip).
    if !is_symlink {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(meta.mode))?;
    }

    // uid/gid — only if root.
    if is_root {
        unsafe {
            let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            libc::lchown(cpath.as_ptr(), meta.uid, meta.gid);
        }
    }

    // mtime — use lchown-style lchown isn't needed here; filetime handles symlinks.
    let ft = FileTime::from_unix_time(meta.mtime_secs, 0);
    if is_symlink {
        // best-effort; not all filesystems support setting symlink mtime
        let _ = filetime::set_symlink_file_times(path, ft, ft);
    } else {
        filetime::set_file_mtime(path, ft)?;
    }

    Ok(())
}
