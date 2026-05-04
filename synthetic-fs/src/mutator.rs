// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// FsMutator: applies random mutations to an FsTree and records what changed

use rand::Rng;

use crate::builder::{random_mtime, random_name, ContentKind, SliceChoose};
use crate::fstree::{EntryMeta, FsEntry, FsTree};

// ── Public mutation types ─────────────────────────────────────────────────────

/// How a file's content was changed.
///
/// Mirrors the B / E / M taxonomy from Tarasov et al. (ATC'12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModKind {
    /// Data prepended at the beginning of the file.
    Prepend,
    /// Data appended at the end of the file.
    Append,
    /// Data changed in the middle (neither first nor last byte changed).
    Middle,
    /// Combination: beginning AND end changed.
    Mixed,
}

/// The kind of change that a single [`MutationRecord`] describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationKind {
    /// A new entry was created at `path`.
    Added,
    /// The entry at `path` was deleted.
    Removed,
    /// The content of the file at `path` was changed.
    Modified { mod_kind: ModKind },
    /// The entry was moved from `from` to `path` (content unchanged).
    Renamed { from: String },
    /// A symlink's target string was changed.
    SymlinkRedirected,
    /// Only metadata (mode and/or mtime) changed; content is the same.
    MetadataOnly,
}

/// A single recorded change applied to an [`FsTree`].
#[derive(Debug, Clone)]
pub struct MutationRecord {
    /// The kind of change.
    pub kind: MutationKind,
    /// The path **after** the mutation (new_path for renames, otherwise same).
    pub path: String,
}

/// The complete log of changes applied by one call to [`FsMutator::mutate`].
pub type MutationLog = Vec<MutationRecord>;

// ── MutationConfig ────────────────────────────────────────────────────────────

/// Relative probabilities for each mutation operation.
///
/// The mutator normalises these weights internally.  Set a weight to 0 to
/// disable that operation.
#[derive(Debug, Clone)]
pub struct MutationConfig {
    /// Delete a random existing file or directory subtree.
    pub weight_delete: u32,
    /// Add a new file.
    pub weight_add_file: u32,
    /// Add a new directory.
    pub weight_add_dir: u32,
    /// Add a new symlink (target = existing file).
    pub weight_add_symlink: u32,
    /// Add a new hardlink to an existing file.
    pub weight_add_hardlink: u32,
    /// Modify the content of an existing file (random ModKind).
    pub weight_modify_file: u32,
    /// Rename a file (content unchanged, new path in same directory).
    pub weight_rename_file: u32,
    /// Rename a directory (moves subtree).
    pub weight_rename_dir: u32,
    /// Redirect a symlink to a different target.
    pub weight_redirect_symlink: u32,
    /// Change only metadata (mode/mtime) of a random entry.
    pub weight_metadata_only: u32,
    /// Minimum number of mutations per call to `mutate`.
    pub min_mutations: usize,
    /// Maximum number of mutations per call to `mutate`.
    pub max_mutations: usize,
}

impl Default for MutationConfig {
    fn default() -> Self {
        Self {
            weight_delete: 10,
            weight_add_file: 20,
            weight_add_dir: 5,
            weight_add_symlink: 5,
            weight_add_hardlink: 5,
            weight_modify_file: 25,
            weight_rename_file: 1,
            weight_rename_dir: 12,
            weight_redirect_symlink: 5,
            weight_metadata_only: 10,
            min_mutations: 3,
            max_mutations: 8,
        }
    }
}

// ── FsMutator ─────────────────────────────────────────────────────────────────

/// Applies random mutations to an [`FsTree`], returning a [`MutationLog`].
///
/// # Example
///
/// ```rust
/// use image_delta_synthetic_fs::{FsTreeBuilder, FsMutator, MutationConfig};
/// use rand::SeedableRng;
/// use rand::rngs::StdRng;
///
/// let mut tree = FsTreeBuilder::new(0).build();
/// let mut rng = StdRng::seed_from_u64(1);
/// let log = FsMutator::new(MutationConfig::default()).mutate(&mut tree, &mut rng);
/// assert!(!log.is_empty());
/// ```
pub struct FsMutator {
    config: MutationConfig,
}

impl FsMutator {
    pub fn new(config: MutationConfig) -> Self {
        Self { config }
    }

    /// Apply a random set of mutations to `tree`, returning the log.
    ///
    /// The number of mutations applied is in `[config.min_mutations,
    /// config.max_mutations]`.
    pub fn mutate(&self, tree: &mut FsTree, rng: &mut impl Rng) -> MutationLog {
        let n = rng.gen_range(self.config.min_mutations..=self.config.max_mutations);
        let mut log = MutationLog::with_capacity(n);

        for _ in 0..n {
            self.apply_one(tree, rng, &mut log);
        }

        log
    }

    fn apply_one(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let op = self.pick_op(rng);

        match op {
            Op::Delete => self.do_delete(tree, rng, log),
            Op::AddFile => self.do_add_file(tree, rng, log),
            Op::AddDir => self.do_add_dir(tree, rng, log),
            Op::AddSymlink => self.do_add_symlink(tree, rng, log),
            Op::AddHardlink => self.do_add_hardlink(tree, rng, log),
            Op::ModifyFile => self.do_modify_file(tree, rng, log),
            Op::RenameFile => self.do_rename_file(tree, rng, log),
            Op::RenameDir => self.do_rename_dir(tree, rng, log),
            Op::RedirectSymlink => self.do_redirect_symlink(tree, rng, log),
            Op::MetadataOnly => self.do_metadata_only(tree, rng, log),
        }
    }

    // ── individual operations ──────────────────────────────────────────────

    fn do_delete(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        // Pick a random file to delete (avoid removing dirs that have children).
        let file_paths = tree.file_paths();
        if file_paths.is_empty() {
            return;
        }
        let path = file_paths[rng.gen_range(0..file_paths.len())].to_owned();

        // Remove any hardlinks pointing to this canonical.
        let hardlinks: Vec<String> = tree
            .entries
            .iter()
            .filter_map(|(p, e)| {
                if let FsEntry::Hardlink { canonical, .. } = e {
                    if *canonical == path {
                        Some(p.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        for hl in &hardlinks {
            tree.remove(hl);
            log.push(MutationRecord {
                kind: MutationKind::Removed,
                path: hl.clone(),
            });
        }

        tree.remove(&path);
        log.push(MutationRecord {
            kind: MutationKind::Removed,
            path,
        });
    }

    fn do_add_file(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let dir_paths = tree.dir_paths();
        if dir_paths.is_empty() {
            return;
        }
        let dir = dir_paths[rng.gen_range(0..dir_paths.len())].to_owned();
        let name = random_name(rng, 3, 10);
        let path = format!("{dir}/{name}");
        if tree.entries.contains_key(&path) {
            return;
        }
        let content = ContentKind::pick(rng).generate(rng);
        let meta = current_uid_meta(rng);
        tree.insert(path.clone(), FsEntry::File { content, meta });
        log.push(MutationRecord {
            kind: MutationKind::Added,
            path,
        });
    }

    fn do_add_dir(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let dir_paths = tree.dir_paths();
        if dir_paths.is_empty() {
            return;
        }
        let parent = dir_paths[rng.gen_range(0..dir_paths.len())].to_owned();
        let name = random_name(rng, 3, 8);
        let path = format!("{parent}/{name}");
        if tree.entries.contains_key(&path) {
            return;
        }
        let meta = current_uid_dir_meta(rng);
        tree.insert(path.clone(), FsEntry::Dir { meta });
        log.push(MutationRecord {
            kind: MutationKind::Added,
            path,
        });
    }

    fn do_add_symlink(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let dir_paths = tree.dir_paths();
        let file_paths = tree.file_paths();
        if dir_paths.is_empty() {
            return;
        }
        let dir = dir_paths[rng.gen_range(0..dir_paths.len())].to_owned();
        let name = format!("link_{}", random_name(rng, 3, 6));
        let path = format!("{dir}/{name}");
        if tree.entries.contains_key(&path) {
            return;
        }
        let target = if !file_paths.is_empty() {
            format!("/{}", file_paths[rng.gen_range(0..file_paths.len())])
        } else {
            format!("/etc/{}", random_name(rng, 3, 8))
        };
        let meta = current_uid_meta(rng);
        tree.insert(path.clone(), FsEntry::Symlink { target, meta });
        log.push(MutationRecord {
            kind: MutationKind::Added,
            path,
        });
    }

    fn do_add_hardlink(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let file_paths = tree.file_paths();
        let dir_paths = tree.dir_paths();
        if file_paths.is_empty() || dir_paths.is_empty() {
            return;
        }
        let canonical = file_paths[rng.gen_range(0..file_paths.len())].to_owned();
        let dir = dir_paths[rng.gen_range(0..dir_paths.len())].to_owned();
        let name = format!("hl_{}", random_name(rng, 3, 6));
        let path = format!("{dir}/{name}");
        if tree.entries.contains_key(&path) || path == canonical {
            return;
        }
        let meta = tree.entries[&canonical].meta().clone();
        tree.insert(path.clone(), FsEntry::Hardlink { canonical, meta });
        log.push(MutationRecord {
            kind: MutationKind::Added,
            path,
        });
    }

    fn do_modify_file(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let file_paths = tree.file_paths();
        if file_paths.is_empty() {
            return;
        }
        let path = file_paths[rng.gen_range(0..file_paths.len())].to_owned();
        let entry = tree.entries.get_mut(&path).unwrap();
        if let FsEntry::File { content, meta } = entry {
            let mod_kind = pick_mod_kind(rng);
            apply_content_mutation(content, &mod_kind, rng);
            meta.mtime_secs = now_secs();
            log.push(MutationRecord {
                kind: MutationKind::Modified {
                    mod_kind: mod_kind.clone(),
                },
                path,
            });
        }
    }

    fn do_rename_file(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let file_paths = tree.file_paths();
        if file_paths.is_empty() {
            return;
        }
        let old_path = file_paths[rng.gen_range(0..file_paths.len())].to_owned();
        // Keep the file in the same directory — only mutate the name.
        let (parent, old_name) = match old_path.rfind('/') {
            Some(pos) => (&old_path[..pos], &old_path[pos + 1..]),
            None => ("", old_path.as_str()),
        };
        let new_name = mutate_name(old_name, rng);
        let new_path = if parent.is_empty() {
            new_name
        } else {
            format!("{parent}/{new_name}")
        };
        if tree.entries.contains_key(&new_path) || new_path == old_path {
            return;
        }

        // Update any hardlinks that point at old_path.
        let hardlinks: Vec<String> = tree
            .entries
            .iter()
            .filter_map(|(p, e)| {
                if let FsEntry::Hardlink { canonical, .. } = e {
                    if *canonical == old_path {
                        Some(p.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        let entry = tree.remove(&old_path).unwrap();
        tree.insert(new_path.clone(), entry);

        // Repoint hardlinks.
        for hl in hardlinks {
            if let Some(FsEntry::Hardlink { canonical, .. }) = tree.entries.get_mut(&hl) {
                *canonical = new_path.clone();
            }
        }

        log.push(MutationRecord {
            kind: MutationKind::Renamed { from: old_path },
            path: new_path,
        });
    }

    fn do_rename_dir(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let dir_paths = tree.dir_paths();
        // Need at least two dirs so we don't rename the only one.
        if dir_paths.len() < 2 {
            return;
        }
        let old_dir = dir_paths[rng.gen_range(0..dir_paths.len())].to_owned();
        // Mutate only the last path component, keeping the parent the same.
        let (parent, old_name) = match old_dir.rfind('/') {
            Some(pos) => (&old_dir[..pos], &old_dir[pos + 1..]),
            None => ("", old_dir.as_str()),
        };
        let new_name = mutate_name(old_name, rng);
        let new_dir = if parent.is_empty() {
            new_name
        } else {
            format!("{parent}/{new_name}")
        };
        if tree.entries.contains_key(&new_dir) || new_dir == old_dir {
            return;
        }

        // Collect all paths that are children of old_dir.
        let to_rename: Vec<String> = tree
            .entries
            .keys()
            .filter(|p| *p == &old_dir || p.starts_with(&format!("{old_dir}/")))
            .cloned()
            .collect();

        for old in &to_rename {
            let new = if *old == old_dir {
                new_dir.clone()
            } else {
                format!("{new_dir}{}", &old[old_dir.len()..])
            };
            if tree.entries.contains_key(&new) {
                continue;
            }
            let entry = tree.remove(old).unwrap();
            // Fixup hardlink canonicals.
            let updated = fix_hardlink_canonical(entry, old, &new);
            tree.insert(new.clone(), updated);
            log.push(MutationRecord {
                kind: MutationKind::Renamed {
                    from: old.to_owned(),
                },
                path: new,
            });
        }

        // Fixup any hardlinks that pointed into the renamed subtree.
        let all_paths: Vec<String> = tree.entries.keys().cloned().collect();
        for p in all_paths {
            if let Some(FsEntry::Hardlink { canonical, .. }) = tree.entries.get_mut(&p) {
                if canonical.starts_with(&format!("{old_dir}/")) {
                    *canonical = format!("{new_dir}{}", &canonical[old_dir.len()..]);
                }
            }
        }
    }

    fn do_redirect_symlink(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let symlink_paths = tree.symlink_paths();
        let file_paths = tree.file_paths();
        if symlink_paths.is_empty() {
            return;
        }
        let path = symlink_paths[rng.gen_range(0..symlink_paths.len())].to_owned();
        let new_target = if !file_paths.is_empty() {
            format!("/{}", file_paths[rng.gen_range(0..file_paths.len())])
        } else {
            format!("/etc/{}", random_name(rng, 3, 8))
        };
        if let Some(FsEntry::Symlink { target, meta }) = tree.entries.get_mut(&path) {
            *target = new_target;
            meta.mtime_secs = now_secs();
        }
        log.push(MutationRecord {
            kind: MutationKind::SymlinkRedirected,
            path,
        });
    }

    fn do_metadata_only(&self, tree: &mut FsTree, rng: &mut impl Rng, log: &mut MutationLog) {
        let paths = tree.paths();
        if paths.is_empty() {
            return;
        }
        let path = paths[rng.gen_range(0..paths.len())].to_owned();
        let is_dir = matches!(tree.entries.get(&path), Some(FsEntry::Dir { .. }));
        let meta = tree.entries.get_mut(&path).unwrap().meta_mut();
        // Randomly flip mode or mtime (or both).
        if rng.gen_bool(0.5) {
            // Any mode bits are allowed for group/other; owner is clamped
            // by sanitize_mode so the current user can always access the entry.
            let raw_modes = [0o644u32, 0o755, 0o600, 0o640, 0o444, 0o750, 0o700, 0o711];
            meta.mode = sanitize_mode(*raw_modes.choose(rng).unwrap_or(&0o644), is_dir);
        }
        if rng.gen_bool(0.5) {
            meta.mtime_secs = random_mtime(rng);
        }
        log.push(MutationRecord {
            kind: MutationKind::MetadataOnly,
            path,
        });
    }

    // ── op picker ─────────────────────────────────────────────────────────────

    fn pick_op(&self, rng: &mut impl Rng) -> Op {
        let c = &self.config;
        let weights = [
            (Op::Delete, c.weight_delete),
            (Op::AddFile, c.weight_add_file),
            (Op::AddDir, c.weight_add_dir),
            (Op::AddSymlink, c.weight_add_symlink),
            (Op::AddHardlink, c.weight_add_hardlink),
            (Op::ModifyFile, c.weight_modify_file),
            (Op::RenameFile, c.weight_rename_file),
            (Op::RenameDir, c.weight_rename_dir),
            (Op::RedirectSymlink, c.weight_redirect_symlink),
            (Op::MetadataOnly, c.weight_metadata_only),
        ];
        let total: u32 = weights.iter().map(|(_, w)| w).sum();
        if total == 0 {
            return Op::ModifyFile;
        }
        let mut r = rng.gen_range(0..total);
        for (op, w) in &weights {
            if r < *w {
                return *op;
            }
            r -= w;
        }
        Op::ModifyFile
    }
}

// ── name mutation ─────────────────────────────────────────────────────────────

/// Produce a realistic variation of `name` rather than a fully random string.
///
/// Strategies (chosen randomly):
/// - **version bump**: if the stem ends in a digit run, increment it
///   (`libfoo-1` → `libfoo-2`, `v3` → `v4`)
/// - **suffix append**: add a common package-style suffix
///   (`.old`, `.bak`, `.new`, `-old`, `-v2`, `-backup`, `~`)
/// - **prefix add**: prepend `new-`, `alt-`, `old-`
/// - **trim/extend**: remove or add one character from the stem
/// - **word swap**: swap one short word for a synonym from a tiny table
///
/// Falls back to appending `-2` if none of the above produced a
/// non-empty, non-identical result.
pub fn mutate_name(name: &str, rng: &mut impl Rng) -> String {
    // Strip a common extension so we mutate just the stem.
    let (stem, ext) = if let Some(dot) = name.rfind('.') {
        // Don't treat a leading dot (hidden files) as an extension.
        if dot > 0 {
            (&name[..dot], &name[dot..]) // e.g. "libfoo.so" → stem="libfoo" ext=".so"
        } else {
            (name, "")
        }
    } else {
        (name, "")
    };

    let strategy = rng.gen_range(0..5u8);
    let new_stem: String = match strategy {
        // ── 0: version bump ───────────────────────────────────────────────────
        0 => {
            // Find the trailing digit run and increment it.
            let trailing_digits = stem
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .count();
            if trailing_digits > 0 {
                let split = stem.len() - trailing_digits;
                let prefix = &stem[..split];
                let num: u64 = stem[split..].parse().unwrap_or(0);
                format!("{prefix}{}", num + 1)
            } else {
                // No trailing digits — append -2.
                format!("{stem}-2")
            }
        }
        // ── 1: suffix append ──────────────────────────────────────────────────
        1 => {
            const SUFFIXES: &[&str] = &[
                ".old", ".bak", ".new", "-old", "-v2", "-backup", "~", "-prev",
            ];
            let suf = SUFFIXES[rng.gen_range(0..SUFFIXES.len())];
            format!("{stem}{suf}")
        }
        // ── 2: prefix add ─────────────────────────────────────────────────────
        2 => {
            const PREFIXES: &[&str] = &["new-", "old-", "alt-", "prev-", "bak-"];
            let pre = PREFIXES[rng.gen_range(0..PREFIXES.len())];
            format!("{pre}{stem}")
        }
        // ── 3: trim / extend one character ────────────────────────────────────
        3 => {
            if stem.len() > 3 && rng.gen_bool(0.5) {
                // Trim the last character.
                stem[..stem.len() - 1].to_string()
            } else {
                // Append one lowercase letter.
                let ch = (b'a' + rng.gen_range(0..26u8)) as char;
                format!("{stem}{ch}")
            }
        }
        // ── 4: word swap ──────────────────────────────────────────────────────
        _ => {
            // A tiny table of common word pairs that appear in path names.
            const SWAPS: &[(&str, &str)] = &[
                ("lib", "library"),
                ("bin", "sbin"),
                ("src", "source"),
                ("tmp", "temp"),
                ("cfg", "config"),
                ("doc", "docs"),
                ("test", "tests"),
                ("log", "logs"),
                ("usr", "user"),
                ("sys", "system"),
                ("app", "apps"),
                ("data", "db"),
            ];
            let mut result = stem.to_string();
            for (a, b) in SWAPS {
                if result.contains(a) {
                    result = result.replacen(a, b, 1);
                    break;
                } else if result.contains(b) {
                    result = result.replacen(b, a, 1);
                    break;
                }
            }
            // If nothing matched, fall back to suffix.
            if result == stem {
                format!("{stem}-2")
            } else {
                result
            }
        }
    };

    if new_stem.is_empty() || new_stem == stem {
        // Last resort fallback.
        format!("{stem}-new{ext}")
    } else {
        format!("{new_stem}{ext}")
    }
}

// ── content mutation helpers ──────────────────────────────────────────────────

fn apply_content_mutation(content: &mut Vec<u8>, kind: &ModKind, rng: &mut impl Rng) {
    let patch_len = rng.gen_range(4..64usize);
    let patch: Vec<u8> = (0..patch_len).map(|_| rng.gen::<u8>()).collect();

    match kind {
        ModKind::Prepend => {
            let mut new = patch;
            new.extend_from_slice(content);
            *content = new;
        }
        ModKind::Append => {
            content.extend_from_slice(&patch);
        }
        ModKind::Middle => {
            if content.len() > 2 {
                let pos = rng.gen_range(1..content.len() - 1);
                let end = (pos + patch_len).min(content.len());
                content.splice(pos..end, patch);
            } else {
                content.extend_from_slice(&patch);
            }
        }
        ModKind::Mixed => {
            // Modify both ends.
            let front: Vec<u8> = (0..patch_len.min(4)).map(|_| rng.gen::<u8>()).collect();
            let back: Vec<u8> = (0..patch_len.min(4)).map(|_| rng.gen::<u8>()).collect();
            let mut new = front;
            new.extend_from_slice(content);
            new.extend_from_slice(&back);
            *content = new;
        }
    }
}

fn pick_mod_kind(rng: &mut impl Rng) -> ModKind {
    match rng.gen_range(0..4u8) {
        0 => ModKind::Prepend,
        1 => ModKind::Append,
        2 => ModKind::Middle,
        _ => ModKind::Mixed,
    }
}

// ── small utilities ───────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Op {
    Delete,
    AddFile,
    AddDir,
    AddSymlink,
    AddHardlink,
    ModifyFile,
    RenameFile,
    RenameDir,
    RedirectSymlink,
    MetadataOnly,
}

/// Enforce minimum owner permissions:
/// - directories: owner must have `rwx` (0o700 mask)
/// - files / symlinks: owner must have at least `rw` (0o600 mask)
///
/// Group and other bits are left untouched.
fn sanitize_mode(mode: u32, is_dir: bool) -> u32 {
    if is_dir {
        mode | 0o700
    } else {
        mode | 0o600
    }
}

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Meta for a newly-added **file or symlink** entry owned by the current user.
fn current_uid_meta(rng: &mut impl Rng) -> EntryMeta {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let raw_modes = [0o644u32, 0o755, 0o600, 0o640, 0o444, 0o750];
    EntryMeta {
        // sanitize_mode ensures owner always has rw (files/symlinks).
        mode: sanitize_mode(*raw_modes.choose(rng).unwrap_or(&0o644), false),
        uid,
        gid,
        mtime_secs: random_mtime(rng),
    }
}

/// Meta for a newly-added **directory** entry owned by the current user.
fn current_uid_dir_meta(rng: &mut impl Rng) -> EntryMeta {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let raw_modes = [0o755u32, 0o750, 0o700, 0o711];
    EntryMeta {
        // sanitize_mode ensures owner always has rwx (dirs).
        mode: sanitize_mode(*raw_modes.choose(rng).unwrap_or(&0o755), true),
        uid,
        gid,
        mtime_secs: random_mtime(rng),
    }
}

fn fix_hardlink_canonical(entry: FsEntry, old: &str, new: &str) -> FsEntry {
    match entry {
        FsEntry::Hardlink { canonical, meta } => {
            let updated_canonical = if canonical == old {
                new.to_string()
            } else {
                canonical
            };
            FsEntry::Hardlink {
                canonical: updated_canonical,
                meta,
            }
        }
        other => other,
    }
}
