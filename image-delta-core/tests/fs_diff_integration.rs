//! Integration tests for [`diff_dirs`].
//!
//! Each test uses a [`Scenario`] builder that creates a real `TempDir` with
//! `base/` and `target/` sub-directories inside it.  Individual tests only
//! describe which files/symlinks/permissions to place in each tree; all
//! diff-running and assertion bookkeeping is handled by shared helpers.
//!
//! Every test that has a clear "forward" semantics also gets a **symmetric**
//! check: the trees are swapped (target becomes base and vice-versa) and
//! Added/Removed swap accordingly.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use filetime::FileTime;
use tempfile::TempDir;

use image_delta_core::fs_diff::{diff_dirs, DiffKind, DiffResult};

// ─────────────────────────────────────────────────────────────────────────────
// Scenario builder
// ─────────────────────────────────────────────────────────────────────────────

/// A pair of `base/` and `target/` directories inside a single `TempDir`.
///
/// All `write_*`, `chmod_*`, `symlink_*` methods return `&Self` so calls can
/// be chained.  The underlying `TempDir` is kept alive for the lifetime of the
/// struct and deleted on drop.
struct Scenario {
    _tmp: TempDir,
    pub base: PathBuf,
    pub target: PathBuf,
}

impl Scenario {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("base");
        let target = tmp.path().join("target");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&target).unwrap();
        Self {
            _tmp: tmp,
            base,
            target,
        }
    }

    // ── File helpers ──────────────────────────────────────────────────────

    fn write_base(&self, rel: &str, content: &[u8]) -> &Self {
        write_file(&self.base, rel, content);
        self
    }

    fn write_target(&self, rel: &str, content: &[u8]) -> &Self {
        write_file(&self.target, rel, content);
        self
    }

    /// Write `rel` to **both** trees with the same content (no diff expected).
    fn write_both(&self, rel: &str, content: &[u8]) -> &Self {
        write_file(&self.base, rel, content);
        write_file(&self.target, rel, content);
        self
    }

    // ── Mode helpers ──────────────────────────────────────────────────────

    fn chmod_base(&self, rel: &str, mode: u32) -> &Self {
        set_mode(&self.base, rel, mode);
        self
    }

    fn chmod_target(&self, rel: &str, mode: u32) -> &Self {
        set_mode(&self.target, rel, mode);
        self
    }

    // ── Symlink helpers ───────────────────────────────────────────────────

    fn symlink_base(&self, link: &str, dest: &str) -> &Self {
        make_symlink(&self.base, link, dest);
        self
    }

    fn symlink_target(&self, link: &str, dest: &str) -> &Self {
        make_symlink(&self.target, link, dest);
        self
    }

    /// Create the **same** symlink in both trees (no diff expected).
    fn symlink_both(&self, link: &str, dest: &str) -> &Self {
        make_symlink(&self.base, link, dest);
        make_symlink(&self.target, link, dest);
        self
    }

    // ── mtime helpers ─────────────────────────────────────────────────────

    /// Set the mtime of `base/rel` to 60 s in the past so that the
    /// mtime-fast-path in `diff_dirs` fires and SHA-256 is compared.
    fn age_base(&self, rel: &str) -> &Self {
        bump_mtime_old(&self.base, rel);
        self
    }

    /// Set the mtime of `target/rel` to 60 s in the past.
    #[allow(dead_code)]
    fn age_target(&self, rel: &str) -> &Self {
        bump_mtime_old(&self.target, rel);
        self
    }

    // ── Hard-link helpers ─────────────────────────────────────────────────

    fn hardlink_base(&self, src: &str, dst: &str) -> &Self {
        fs::hard_link(self.base.join(src), self.base.join(dst)).unwrap();
        self
    }

    fn hardlink_target(&self, src: &str, dst: &str) -> &Self {
        fs::hard_link(self.target.join(src), self.target.join(dst)).unwrap();
        self
    }

    // ── Directory helpers ─────────────────────────────────────────────────

    fn mkdir_base(&self, rel: &str) -> &Self {
        fs::create_dir_all(self.base.join(rel)).unwrap();
        self
    }

    fn mkdir_target(&self, rel: &str) -> &Self {
        fs::create_dir_all(self.target.join(rel)).unwrap();
        self
    }

    // ── Diff runners ──────────────────────────────────────────────────────

    fn diff_forward(&self) -> DiffResult {
        diff_dirs(&self.base, &self.target).expect("diff_dirs forward failed")
    }

    fn diff_reverse(&self) -> DiffResult {
        diff_dirs(&self.target, &self.base).expect("diff_dirs reverse failed")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Low-level filesystem helpers
// ─────────────────────────────────────────────────────────────────────────────

fn write_file(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, content).unwrap();
}

fn set_mode(root: &Path, rel: &str, mode: u32) {
    let p = root.join(rel);
    fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
}

fn make_symlink(root: &Path, link: &str, dest: &str) {
    let p = root.join(link);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    symlink(dest, &p).unwrap();
}

/// Set mtime of `root/rel` to 60 seconds in the past.
fn bump_mtime_old(root: &Path, rel: &str) {
    let p = root.join(rel);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    filetime::set_file_mtime(&p, FileTime::from_unix_time(now - 60, 0)).unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// Assertion helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Assert that `result` contains **exactly** the diffs in `expected` —
/// no more, no fewer.
fn assert_exact_diffs(label: &str, result: &DiffResult, expected: &[(&str, DiffKind)]) {
    // Check every expected diff is present with the right kind.
    for (path, kind) in expected {
        let found = result.diffs.iter().find(|d| d.path == *path);
        assert!(
            found.is_some(),
            "[{label}] expected diff for '{path}' ({kind:?}) but not found\ndiffs: {:#?}",
            result.diffs,
        );
        assert_eq!(
            found.unwrap().kind,
            *kind,
            "[{label}] wrong kind for '{path}'\ndiffs: {:#?}",
            result.diffs,
        );
    }

    // Check there are no *unexpected* diffs.
    for diff in &result.diffs {
        let is_expected = expected.iter().any(|(p, _)| *p == diff.path);
        assert!(
            is_expected,
            "[{label}] unexpected diff for '{}' ({:?})\ndiffs: {:#?}",
            diff.path, diff.kind, result.diffs,
        );
    }

    assert_eq!(
        result.diffs.len(),
        expected.len(),
        "[{label}] diff count mismatch\ndiffs: {:#?}",
        result.diffs,
    );
}

/// Build the mirrored expectation for the reverse (base↔target swapped) direction.
///
/// - `Added`        → `Removed`
/// - `Removed`      → `Added`
/// - `Changed`      → `Changed`
/// - `MetadataOnly` → `MetadataOnly`
fn mirror<'a>(expected: &[(&'a str, DiffKind)]) -> Vec<(&'a str, DiffKind)> {
    expected
        .iter()
        .map(|(p, k)| {
            let mirrored = match k {
                DiffKind::Added => DiffKind::Removed,
                DiffKind::Removed => DiffKind::Added,
                DiffKind::Changed => DiffKind::Changed,
                DiffKind::MetadataOnly => DiffKind::MetadataOnly,
            };
            (*p, mirrored)
        })
        .collect()
}

/// Run [`assert_exact_diffs`] in both directions.
fn assert_symmetric(s: &Scenario, expected: &[(&str, DiffKind)]) {
    assert_exact_diffs("forward", &s.diff_forward(), expected);
    let rev_expected = mirror(expected);
    assert_exact_diffs("reverse", &s.diff_reverse(), &rev_expected);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

/// All four diff kinds present in a single scenario — no false positives.
///
/// Tree layout:
///   unchanged.txt   — identical in both   → no diff
///   added.txt       — only in target       → Added
///   removed.txt     — only in base         → Removed
///   changed.txt     — different content    → Changed
///   meta.sh         — same content, chmod  → MetadataOnly
#[test]
fn test_all_four_diff_kinds() {
    let s = Scenario::new();

    s.write_both("unchanged.txt", b"same content stays same")
        .write_target("added.txt", b"brand new file")
        .write_base("removed.txt", b"this will disappear")
        .write_base("changed.txt", b"old content, quite different from new")
        .write_target("changed.txt", b"new content, quite different from old")
        // age base so mtime differs → SHA-256 comparison triggers
        .age_base("changed.txt")
        .write_both("meta.sh", b"#!/bin/sh\necho hi\n")
        .chmod_base("meta.sh", 0o644)
        .chmod_target("meta.sh", 0o755);

    assert_symmetric(
        &s,
        &[
            ("added.txt", DiffKind::Added),
            ("removed.txt", DiffKind::Removed),
            ("changed.txt", DiffKind::Changed),
            ("meta.sh", DiffKind::MetadataOnly),
        ],
    );
}

/// Files nested several levels deep are walked correctly.
///
/// Tree layout:
///   usr/bin/ls              — unchanged
///   usr/bin/grep            — changed (binary content)
///   usr/lib/libfoo-2.31.so  — removed (rename → also added as 2.35)
///   usr/lib/libfoo-2.35.so  — added
///   etc/passwd              — changed
///   etc/shadow              — unchanged
///   etc/sudoers             — mode-only change
#[test]
fn test_nested_package_upgrade() {
    let s = Scenario::new();

    s.write_both("usr/bin/ls", &vec![0xEF; 1024])
        .write_base("usr/bin/grep", &vec![0xAB; 2048])
        .write_target("usr/bin/grep", &vec![0xCD; 2048])
        .age_base("usr/bin/grep")
        .write_base("usr/lib/libfoo-2.31.so", &vec![0x7F, 0x45, 0x4C, 0x46])
        .write_target("usr/lib/libfoo-2.35.so", &vec![0x7F, 0x45, 0x4C, 0x46])
        .write_base("etc/passwd", b"root:x:0:0:root:/root:/bin/bash\n")
        .write_target(
            "etc/passwd",
            b"root:x:0:0:root:/root:/bin/bash\nnewuser:x:1001:1001::/home/newuser:/bin/sh\n",
        )
        .age_base("etc/passwd")
        .write_both("etc/shadow", b"root:!locked:::::::\n")
        .write_both("etc/sudoers", b"root ALL=(ALL:ALL) ALL\n")
        .chmod_base("etc/sudoers", 0o640)
        .chmod_target("etc/sudoers", 0o440);

    assert_symmetric(
        &s,
        &[
            ("usr/bin/grep", DiffKind::Changed),
            ("usr/lib/libfoo-2.31.so", DiffKind::Removed),
            ("usr/lib/libfoo-2.35.so", DiffKind::Added),
            ("etc/passwd", DiffKind::Changed),
            ("etc/sudoers", DiffKind::MetadataOnly),
        ],
    );
}

/// Symlinks: added, removed, changed target, unchanged, and a type change
/// (regular file in base → symlink in target at same path).
#[test]
fn test_symlink_variety() {
    let s = Scenario::new();

    // Identical symlink → no diff.
    s.symlink_both("same_link", "/usr/lib/libssl.so.3")
        // Symlink only in target.
        .symlink_target("new_link", "/usr/bin/python3")
        // Symlink only in base.
        .symlink_base("old_link", "/usr/bin/python2")
        // Symlink whose target changes.
        .symlink_base("changed_link", "/lib/libz.so.1")
        .symlink_target("changed_link", "/lib/x86_64-linux-gnu/libz.so.1")
        // Regular file in base → symlink in target at the same path (type change).
        .write_base("resolv.conf", b"nameserver 1.1.1.1\n")
        .symlink_target("resolv.conf", "../run/systemd/resolve/stub-resolv.conf");

    assert_symmetric(
        &s,
        &[
            ("new_link", DiffKind::Added),
            ("old_link", DiffKind::Removed),
            ("changed_link", DiffKind::Changed),
            ("resolv.conf", DiffKind::Changed), // type change counts as Changed
        ],
    );
}

/// A whole new sub-tree appearing in target, and a whole sub-tree disappearing
/// from base.  Every entry inside the added/removed sub-tree must be reported.
#[test]
fn test_subtree_added_and_removed() {
    let s = Scenario::new();

    // A sub-tree only in base (to be removed).
    s.write_base("old_pkg/bin/tool", b"old binary")
        .write_base("old_pkg/lib/helper.so", b"old lib")
        .symlink_base("old_pkg/bin/tool-link", "tool");

    // A sub-tree only in target (to be added).
    s.write_target("new_pkg/bin/tool", b"new binary")
        .write_target("new_pkg/lib/helper.so", b"new lib")
        .symlink_target("new_pkg/doc/README", "../../README.md");

    // A shared file that stays unchanged.
    s.write_both("shared/config.conf", b"key=value\n");

    assert_symmetric(
        &s,
        &[
            // base sub-tree vanishes
            ("old_pkg", DiffKind::Removed),
            ("old_pkg/bin", DiffKind::Removed),
            ("old_pkg/bin/tool", DiffKind::Removed),
            ("old_pkg/bin/tool-link", DiffKind::Removed),
            ("old_pkg/lib", DiffKind::Removed),
            ("old_pkg/lib/helper.so", DiffKind::Removed),
            // target sub-tree appears
            ("new_pkg", DiffKind::Added),
            ("new_pkg/bin", DiffKind::Added),
            ("new_pkg/bin/tool", DiffKind::Added),
            ("new_pkg/lib", DiffKind::Added),
            ("new_pkg/lib/helper.so", DiffKind::Added),
            ("new_pkg/doc", DiffKind::Added),
            ("new_pkg/doc/README", DiffKind::Added),
        ],
    );
}

/// Directory attribute changes (mode) are reported as MetadataOnly.
/// Adding a new empty directory is reported as Added.
/// Removing an empty directory is reported as Removed.
#[test]
fn test_directory_attribute_changes() {
    let s = Scenario::new();

    // A directory whose permissions tighten.
    s.mkdir_base("secure_dir")
        .mkdir_target("secure_dir")
        .chmod_base("secure_dir", 0o755)
        .chmod_target("secure_dir", 0o700);

    // A directory that disappears entirely.
    s.mkdir_base("gone_dir");

    // A directory that appears fresh.
    s.mkdir_target("new_dir");

    assert_symmetric(
        &s,
        &[
            ("secure_dir", DiffKind::MetadataOnly),
            ("gone_dir", DiffKind::Removed),
            ("new_dir", DiffKind::Added),
        ],
    );
}

/// Hard-link changes: the relationship is preserved in both trees → no diff.
/// Breaking a hard-link (one side has nlink=1) is detected by `compare_dirs`
/// in the test suite but is NOT currently a diff kind in `diff_dirs` itself
/// (hard-link grouping is used only for compressor deduplication).
/// This test documents that *content equality* still means no `Changed` diff.
#[test]
fn test_hardlinks_content_unchanged() {
    let s = Scenario::new();

    // Same hard-link pair in both trees.
    s.write_both("data.bin", &vec![0xDE; 512]);
    s.hardlink_base("data.bin", "data_link.bin");
    s.hardlink_target("data.bin", "data_link.bin");

    assert_symmetric(&s, &[]); // zero diffs — content is identical
}

/// Realistic init-system scenario: several units changed, one removed, one
/// added.  The point is that unchanged units produce zero noise.
#[test]
fn test_systemd_unit_upgrade() {
    let s = Scenario::new();

    // Units that do not change.
    for name in &["sshd.service", "cron.service", "dbus.service"] {
        s.write_both(
            &format!("lib/systemd/system/{name}"),
            format!("[Unit]\nDescription={name}\n").as_bytes(),
        );
    }

    // Unit whose content changes.
    s.write_base(
        "lib/systemd/system/NetworkManager.service",
        b"[Unit]\nDescription=NetworkManager old\n",
    )
    .write_target(
        "lib/systemd/system/NetworkManager.service",
        b"[Unit]\nDescription=NetworkManager new\n[Install]\nWantedBy=multi-user.target\n",
    )
    .age_base("lib/systemd/system/NetworkManager.service");

    // Unit removed (deprecated in new image).
    s.write_base(
        "lib/systemd/system/ifupdown.service",
        b"[Unit]\nDescription=ifupdown\n",
    );

    // Unit added (new service in new image).
    s.write_target(
        "lib/systemd/system/systemd-resolved.service",
        b"[Unit]\nDescription=Network Name Resolution\n",
    );

    // A mode-only change on one unit.
    s.write_both(
        "lib/systemd/system/rsyslog.service",
        b"[Unit]\nDescription=rsyslog\n",
    )
    .chmod_base("lib/systemd/system/rsyslog.service", 0o644)
    .chmod_target("lib/systemd/system/rsyslog.service", 0o444);

    assert_symmetric(
        &s,
        &[
            (
                "lib/systemd/system/NetworkManager.service",
                DiffKind::Changed,
            ),
            ("lib/systemd/system/ifupdown.service", DiffKind::Removed),
            (
                "lib/systemd/system/systemd-resolved.service",
                DiffKind::Added,
            ),
            ("lib/systemd/system/rsyslog.service", DiffKind::MetadataOnly),
        ],
    );
}

/// Combination of content change + mode change on the same file.
/// Content change takes priority and the entry must be reported as `Changed`,
/// NOT `MetadataOnly`.
#[test]
fn test_content_and_mode_change_is_changed_not_metadata() {
    let s = Scenario::new();

    s.write_base("script.sh", b"#!/bin/sh\necho old\n")
        .write_target("script.sh", b"#!/bin/sh\necho new\n")
        .age_base("script.sh")
        .chmod_base("script.sh", 0o644)
        .chmod_target("script.sh", 0o755);

    assert_symmetric(&s, &[("script.sh", DiffKind::Changed)]);
}

/// Symlinks are not followed.  A symlink pointing to a populated directory
/// outside the tree must appear as a single entry — its target's contents
/// must NOT bleed into the diff.
#[test]
fn test_symlinks_are_not_followed_into_external_tree() {
    let outside = TempDir::new().unwrap();
    write_file(outside.path(), "secret.txt", b"should not appear");
    write_file(outside.path(), "another.txt", b"should not appear either");

    let s = Scenario::new();
    // Same symlink → no diff, but the outer dir contents must not be walked.
    s.symlink_both("link_to_outside", outside.path().to_str().unwrap());

    let fwd = s.diff_forward();
    let leaked = fwd
        .diffs
        .iter()
        .any(|d| d.path.contains("secret") || d.path.contains("another"));
    assert!(!leaked, "symlink target was followed: {:#?}", fwd.diffs);
    assert!(
        fwd.is_empty(),
        "identical symlinks should produce no diffs: {:#?}",
        fwd.diffs
    );
}

/// `TreeStats` sanity: counts and byte totals in `DiffResult` are consistent
/// with the number of files actually written.
#[test]
fn test_tree_stats_are_populated() {
    let s = Scenario::new();

    // base: 3 regular files, 1 symlink, 1 subdir
    s.write_base("a.txt", &vec![0u8; 100])
        .write_base("b.txt", &vec![0u8; 200])
        .write_base("sub/c.txt", &vec![0u8; 300]) // sub/ is auto-created
        .symlink_base("link", "a.txt");

    // target: 2 regular files (a.txt + d.txt), 1 symlink
    s.write_target("a.txt", &vec![0u8; 100])
        .write_target("d.txt", &vec![0u8; 400])
        .symlink_target("link", "a.txt");

    let result = s.diff_forward();

    assert_eq!(result.base.files, 3, "base should have 3 files");
    assert_eq!(result.base.symlinks, 1, "base should have 1 symlink");
    assert_eq!(result.base.dirs, 1, "base should have 1 subdir");
    assert_eq!(result.base.total_bytes, 600, "base total bytes");

    assert_eq!(result.target.files, 2, "target should have 2 files");
    assert_eq!(result.target.symlinks, 1, "target should have 1 symlink");
    assert_eq!(result.target.dirs, 0, "target should have 0 subdirs");
    assert_eq!(result.target.total_bytes, 500, "target total bytes");
}
