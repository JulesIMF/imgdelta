// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Unit tests for image-delta-synthetic-fs

use rand::rngs::StdRng;
use rand::SeedableRng;

use image_delta_synthetic_fs::mutator::MutationConfig;
use image_delta_synthetic_fs::{FsMutator, FsTreeBuilder};

// ── FsTreeBuilder ─────────────────────────────────────────────────────────────

#[test]
fn builder_produces_nonempty_tree() {
    let tree = FsTreeBuilder::new(0).build();
    assert!(tree.len() >= 20, "expected ≥20 entries, got {}", tree.len());
}

#[test]
fn builder_is_deterministic() {
    let a = FsTreeBuilder::new(123).build();
    let b = FsTreeBuilder::new(123).build();
    let mut paths_a: Vec<_> = a.paths().iter().map(|s| s.to_string()).collect();
    let mut paths_b: Vec<_> = b.paths().iter().map(|s| s.to_string()).collect();
    paths_a.sort();
    paths_b.sort();
    assert_eq!(paths_a, paths_b);
}

#[test]
fn builder_different_seeds_differ() {
    let a = FsTreeBuilder::new(0).build();
    let b = FsTreeBuilder::new(1).build();
    let paths_a: std::collections::HashSet<_> = a.paths().iter().map(|s| s.to_string()).collect();
    let paths_b: std::collections::HashSet<_> = b.paths().iter().map(|s| s.to_string()).collect();
    assert_ne!(paths_a, paths_b);
}

#[test]
fn builder_entries_count_range() {
    for seed in 0u64..10 {
        let tree = FsTreeBuilder::new(seed).with_entry_count(5, 10).build();
        let len = tree.len();
        // Allow small slack (symlinks/hardlinks may be skipped if target pool is small).
        assert!(len >= 3, "seed {seed}: expected len≥3, got {len}");
    }
}

#[test]
fn builder_write_to_dir_and_read_back() {
    let tempdir = tempfile::tempdir().unwrap();
    let tree = FsTreeBuilder::new(42).build();
    tree.write_to_dir(tempdir.path()).unwrap();

    let restored = image_delta_synthetic_fs::FsTree::from_dir(tempdir.path()).unwrap();

    // All regular files and dirs should be present.
    for path in tree.file_paths() {
        assert!(
            restored.entries.contains_key(path),
            "missing after round-trip: {path}"
        );
    }
    for path in tree.dir_paths() {
        assert!(
            restored.entries.contains_key(path),
            "missing dir after round-trip: {path}"
        );
    }
}

// ── FsMutator ─────────────────────────────────────────────────────────────────

#[test]
fn mutator_produces_nonempty_log() {
    let mut tree = FsTreeBuilder::new(0).build();
    let mut rng = StdRng::seed_from_u64(1);
    let log = FsMutator::new(MutationConfig::default()).mutate(&mut tree, &mut rng);
    assert!(!log.is_empty(), "mutation log should not be empty");
}

#[test]
fn mutator_is_deterministic() {
    let base = FsTreeBuilder::new(7).build();

    let mut tree_a = base.clone();
    let mut rng_a = StdRng::seed_from_u64(99);
    let log_a = FsMutator::new(MutationConfig::default()).mutate(&mut tree_a, &mut rng_a);

    let mut tree_b = base.clone();
    let mut rng_b = StdRng::seed_from_u64(99);
    let log_b = FsMutator::new(MutationConfig::default()).mutate(&mut tree_b, &mut rng_b);

    assert_eq!(log_a.len(), log_b.len());
    let mut pa: Vec<_> = tree_a.paths().iter().map(|s| s.to_string()).collect();
    let mut pb: Vec<_> = tree_b.paths().iter().map(|s| s.to_string()).collect();
    pa.sort();
    pb.sort();
    assert_eq!(pa, pb);
}

#[test]
fn mutator_hardlinks_remain_valid_after_mutation() {
    use image_delta_synthetic_fs::FsEntry;

    let mut tree = FsTreeBuilder::new(5).build();
    let mut rng = StdRng::seed_from_u64(3);
    let cfg = MutationConfig {
        min_mutations: 10,
        max_mutations: 20,
        ..MutationConfig::default()
    };
    FsMutator::new(cfg).mutate(&mut tree, &mut rng);

    // Every hardlink must point to an existing canonical entry.
    for (path, entry) in &tree.entries {
        if let FsEntry::Hardlink { canonical, .. } = entry {
            assert!(
                tree.entries.contains_key(canonical.as_str()),
                "hardlink at {path} points to missing canonical {canonical}"
            );
        }
    }
}

#[test]
fn mutator_mutate_multiple_rounds() {
    let mut tree = FsTreeBuilder::new(10).build();
    let mut rng = StdRng::seed_from_u64(0);
    let cfg = MutationConfig::default();
    let mutator = FsMutator::new(cfg);

    for _ in 0..5 {
        let log = mutator.mutate(&mut tree, &mut rng);
        assert!(!log.is_empty());
    }
    assert!(!tree.is_empty());
}

#[test]
fn mutator_write_to_dir_after_mutations() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut tree = FsTreeBuilder::new(20).build();
    let mut rng = StdRng::seed_from_u64(20);

    FsMutator::new(MutationConfig::default()).mutate(&mut tree, &mut rng);

    tree.write_to_dir(tempdir.path()).unwrap();

    // Basic sanity: at least the regular files exist on disk.
    for path in tree.file_paths() {
        let disk_path = tempdir.path().join(path);
        assert!(disk_path.exists(), "file missing on disk: {path}");
    }
}
