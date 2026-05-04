// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// FsTreeBuilder: generates random initial filesystem snapshots

use std::time::{SystemTime, UNIX_EPOCH};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::fstree::{EntryMeta, FsEntry, FsTree};

// ── content helpers ───────────────────────────────────────────────────────────

/// Varieties of file content to generate.
///
/// The builder picks proportionally: ~20 % empty, ~20 % small text,
/// ~20 % large text, ~20 % small binary, ~20 % large binary.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ContentKind {
    Empty,
    SmallText,
    LargeText,
    SmallBinary,
    LargeBinary,
}

impl ContentKind {
    pub(crate) fn pick(rng: &mut impl Rng) -> Self {
        match rng.gen_range(0..5u8) {
            0 => Self::Empty,
            1 => Self::SmallText,
            2 => Self::LargeText,
            3 => Self::SmallBinary,
            _ => Self::LargeBinary,
        }
    }

    pub(crate) fn generate(self, rng: &mut impl Rng) -> Vec<u8> {
        match self {
            Self::Empty => vec![],
            Self::SmallText => {
                let n = rng.gen_range(8..512usize);
                random_text(rng, n)
            }
            Self::LargeText => {
                let n = rng.gen_range(16 * 1024..80 * 1024usize);
                random_text(rng, n)
            }
            Self::SmallBinary => {
                let n = rng.gen_range(8..512usize);
                random_bytes(rng, n)
            }
            Self::LargeBinary => {
                let n = rng.gen_range(16 * 1024..80 * 1024usize);
                random_bytes(rng, n)
            }
        }
    }
}

fn random_text(rng: &mut impl Rng, len: usize) -> Vec<u8> {
    const CHARS: &[u8] =
        b"abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\n\t.,;:!?-_/=";
    (0..len)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())])
        .collect()
}

fn random_bytes(rng: &mut impl Rng, len: usize) -> Vec<u8> {
    (0..len).map(|_| rng.gen::<u8>()).collect()
}

// ── FsTreeBuilder ─────────────────────────────────────────────────────────────

/// Builds a random initial [`FsTree`] suitable as the base image in a chain.
///
/// # Example
///
/// ```rust
/// use image_delta_synthetic_fs::FsTreeBuilder;
///
/// let tree = FsTreeBuilder::new(42).build();
/// assert!(tree.len() >= 20);
/// ```
pub struct FsTreeBuilder {
    seed: u64,
    min_entries: usize,
    max_entries: usize,
    uid: u32,
    gid: u32,
    allow_hardlinks: bool,
}

impl FsTreeBuilder {
    /// Create a builder with a fixed random seed.  All other parameters use
    /// sensible defaults (20–40 entries, current process uid/gid).
    pub fn new(seed: u64) -> Self {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        Self {
            seed,
            min_entries: 20,
            max_entries: 40,
            uid,
            gid,
            allow_hardlinks: true,
        }
    }

    /// Disable hardlink generation.
    ///
    /// Useful for round-trip tests that don't preserve hardlink inode
    /// relationships (e.g. when delta-compression copies unchanged files
    /// as independent inodes).
    pub fn with_hardlinks(mut self, allow: bool) -> Self {
        self.allow_hardlinks = allow;
        self
    }

    /// Override the uid stored in generated metadata (root-only scenarios).
    pub fn with_uid(mut self, uid: u32) -> Self {
        self.uid = uid;
        self
    }

    /// Override the gid stored in generated metadata.
    pub fn with_gid(mut self, gid: u32) -> Self {
        self.gid = gid;
        self
    }

    /// Override the entry count range (inclusive on both ends).
    pub fn with_entry_count(mut self, min: usize, max: usize) -> Self {
        assert!(min <= max && min > 0);
        self.min_entries = min;
        self.max_entries = max;
        self
    }

    /// Generate the `FsTree`.
    pub fn build(&self) -> FsTree {
        let mut rng = StdRng::seed_from_u64(self.seed);
        let mut tree = FsTree::new();

        let target_entries = rng.gen_range(self.min_entries..=self.max_entries);

        // ── 1. Generate a pool of directories ────────────────────────────────
        // Always create a small set of dirs; files will be placed inside them.
        let n_dirs = rng.gen_range(3..=8usize).min(target_entries / 3);
        let mut dir_paths: Vec<String> = Vec::with_capacity(n_dirs + 1);

        // The root-level "." is implicit; we add top-level dirs.
        let top_names = ["bin", "etc", "lib", "usr", "var", "opt", "tmp", "srv"];
        for name in top_names.iter().take(n_dirs) {
            let path = name.to_string();
            let meta = self.random_dir_meta(&mut rng);
            tree.insert(path.clone(), FsEntry::Dir { meta });
            dir_paths.push(path);
        }

        // Add a couple of nested subdirs.
        let nested = rng.gen_range(0..=3usize);
        for _ in 0..nested {
            let parent = dir_paths[rng.gen_range(0..dir_paths.len())].clone();
            let name = random_name(&mut rng, 4, 8);
            let path = format!("{parent}/{name}");
            if !tree.entries.contains_key(&path) {
                let meta = self.random_dir_meta(&mut rng);
                tree.insert(path.clone(), FsEntry::Dir { meta });
                dir_paths.push(path);
            }
        }

        // ── 2. Generate files until we hit the target count ──────────────────
        let remaining = target_entries.saturating_sub(tree.len());
        let n_files = (remaining * 7 / 10).max(5); // ~70 % files
        let mut file_paths: Vec<String> = Vec::with_capacity(n_files);

        for _ in 0..n_files {
            let dir = &dir_paths[rng.gen_range(0..dir_paths.len())];
            let name = random_name(&mut rng, 3, 12);
            let path = format!("{dir}/{name}");
            if tree.entries.contains_key(&path) {
                continue;
            }
            let content = ContentKind::pick(&mut rng).generate(&mut rng);
            let meta = self.random_file_meta(&mut rng);
            tree.insert(path.clone(), FsEntry::File { content, meta });
            file_paths.push(path);
        }

        // ── 3. Add symlinks (~15 % of remaining budget) ──────────────────────
        let remaining = target_entries.saturating_sub(tree.len());
        let n_symlinks = (remaining / 3).min(5);
        for _ in 0..n_symlinks {
            let dir = &dir_paths[rng.gen_range(0..dir_paths.len())];
            let name = format!("link_{}", random_name(&mut rng, 3, 6));
            let path = format!("{dir}/{name}");
            if tree.entries.contains_key(&path) {
                continue;
            }
            // Point to a random existing file, or an arbitrary path.
            let target = if !file_paths.is_empty() {
                format!("/{}", file_paths[rng.gen_range(0..file_paths.len())])
            } else {
                format!("/etc/{}", random_name(&mut rng, 4, 8))
            };
            let meta = self.random_file_meta(&mut rng);
            tree.insert(path, FsEntry::Symlink { target, meta });
        }

        // ── 4. Add hardlinks (~15 % of remaining budget) ─────────────────────
        if self.allow_hardlinks && file_paths.len() >= 2 {
            let remaining = target_entries.saturating_sub(tree.len());
            let n_hardlinks = (remaining / 2).min(4);
            for _ in 0..n_hardlinks {
                let canonical = file_paths[rng.gen_range(0..file_paths.len())].clone();
                let dir = &dir_paths[rng.gen_range(0..dir_paths.len())];
                let name = format!("hl_{}", random_name(&mut rng, 3, 6));
                let path = format!("{dir}/{name}");
                if tree.entries.contains_key(&path) || path == canonical {
                    continue;
                }
                let meta = tree.entries[&canonical].meta().clone();
                tree.insert(path, FsEntry::Hardlink { canonical, meta });
            }
        }

        tree
    }

    fn random_file_meta(&self, rng: &mut impl Rng) -> EntryMeta {
        // Owner must always have rw (bits 0o600): 0o444 is excluded.
        let mode = *[0o644u32, 0o755, 0o600, 0o640, 0o750].choose(rng).unwrap();
        EntryMeta {
            mode,
            uid: self.uid,
            gid: self.gid,
            mtime_secs: random_mtime(rng),
        }
    }

    fn random_dir_meta(&self, rng: &mut impl Rng) -> EntryMeta {
        let mode = *[0o755u32, 0o750, 0o700].choose(rng).unwrap();
        EntryMeta {
            mode,
            uid: self.uid,
            gid: self.gid,
            mtime_secs: random_mtime(rng),
        }
    }
}

// ── utilities ─────────────────────────────────────────────────────────────────

/// Returns a random mtime within ±1 year of "now – 30 seconds".
///
/// The –30 s offset ensures that base files appear older than freshly-created
/// target files (needed for mtime-based change detection in the compressor).
pub fn random_mtime(rng: &mut impl Rng) -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let offset = rng.gen_range(-365 * 24 * 3600i64..=(-30));
    now + offset
}

/// Generate a random alphanumeric name of length in `[min, max]`.
pub fn random_name(rng: &mut impl Rng, min: usize, max: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let len = rng.gen_range(min..=max);
    (0..len)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

// ── SliceRng helper (for .choose()) ──────────────────────────────────────────

pub(crate) trait SliceChoose {
    type Item;
    fn choose(&self, rng: &mut impl Rng) -> Option<&Self::Item>;
}

impl<T> SliceChoose for [T] {
    type Item = T;
    fn choose(&self, rng: &mut impl Rng) -> Option<&T> {
        if self.is_empty() {
            None
        } else {
            Some(&self[rng.gen_range(0..self.len())])
        }
    }
}
