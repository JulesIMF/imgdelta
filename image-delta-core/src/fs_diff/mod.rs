use std::path::Path;

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
    Modified,
    /// Content is identical but metadata (mode / uid / gid / mtime) changed.
    MetadataOnly,
}

/// Result of comparing two directory trees.
#[derive(Debug, Default)]
pub struct DiffResult {
    pub diffs: Vec<FileDiff>,
}

impl DiffResult {
    pub fn added(&self) -> impl Iterator<Item = &FileDiff> {
        self.diffs.iter().filter(|d| d.kind == DiffKind::Added)
    }

    pub fn removed(&self) -> impl Iterator<Item = &FileDiff> {
        self.diffs.iter().filter(|d| d.kind == DiffKind::Removed)
    }

    pub fn modified(&self) -> impl Iterator<Item = &FileDiff> {
        self.diffs.iter().filter(|d| d.kind == DiffKind::Modified)
    }
}

/// Walk two directory trees and compute their differences.
///
/// Compares:
/// - SHA-256 of file contents
/// - Unix mode, uid, gid
/// - mtime (with ±1 s tolerance for filesystem rounding)
/// - Symlink targets
/// - Hard-link relationships
///
/// # Errors
///
/// Returns [`crate::Error::Io`] if either tree cannot be read.
pub fn diff_dirs(base: &Path, target: &Path) -> crate::Result<DiffResult> {
    todo!("Phase 3: walkdir + SHA-256 comparison")
}
