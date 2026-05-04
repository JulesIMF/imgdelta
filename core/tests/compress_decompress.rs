// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Round-trip integration tests: compress then decompress and compare trees

// Phase 6.E: decompress is now implemented.
mod common;

use common::{
    compare_dirs, compress_opts, decompress_opts, make_compressor, set_mode, set_mtime_old,
    write_file, write_symlink,
};
use image_delta_core::{Compressor, ImageMeta, Storage};
use tempfile::tempdir;

// ── Helper: save root image meta so chain-check passes ────────────────────────

async fn save_root_meta(storage: &dyn Storage, image_id: &str) {
    storage
        .register_image(&ImageMeta {
            image_id: image_id.to_string(),
            base_image_id: None,
            format: "directory".into(),
            status: "active".into(),
        })
        .await
        .unwrap();
}

// ── 1. test_roundtrip_simple ──────────────────────────────────────────────────

/// base = {file_a, file_b, file_c}; target = {file_a changed, file_c unchanged,
/// file_d new, file_b removed}.  After compress + decompress, output == target.
#[tokio::test]
async fn test_roundtrip_simple() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(base.path(), "file_a.txt", b"hello world original");
    set_mtime_old(base.path(), "file_a.txt");
    write_file(base.path(), "file_b.txt", b"will be removed");
    write_file(base.path(), "file_c.txt", b"unchanged content");

    write_file(
        target.path(),
        "file_a.txt",
        b"hello world updated version 2",
    );
    write_file(target.path(), "file_c.txt", b"unchanged content");
    write_file(target.path(), "file_d.txt", b"brand new file");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base").await;

    let stats = compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-1", Some("img-base")),
        )
        .await
        .unwrap();

    let decomp = compressor
        .decompress(output.path(), decompress_opts("img-1", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(diffs.is_empty(), "round-trip failed:\n{diffs:#?}");

    // Stats sanity
    assert!(
        stats.files_patched + stats.files_added + stats.files_removed > 0,
        "no stats recorded: {stats:?}"
    );
    let _ = decomp; // elapsed_secs etc.
}

// ── 2. test_roundtrip_rename ──────────────────────────────────────────────────

/// A file is renamed with identical content — path_match detects the rename.
#[tokio::test]
async fn test_roundtrip_rename() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // Version-bump rename: libc-2.31.so → libc-2.35.so, same content.
    write_file(
        base.path(),
        "lib/libc-2.31.so",
        b"ELF libc binary placeholder",
    );
    write_file(
        target.path(),
        "lib/libc-2.35.so",
        b"ELF libc binary placeholder",
    );

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-rename", Some("img-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-rename", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(diffs.is_empty(), "rename round-trip failed:\n{diffs:#?}");
}

// ── 3. test_roundtrip_metadata_only ──────────────────────────────────────────

/// Only the mode changes — no content diff.
#[tokio::test]
async fn test_roundtrip_metadata_only() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(base.path(), "script.sh", b"#!/bin/sh\necho hello\n");
    set_mode(base.path(), "script.sh", 0o644);

    write_file(target.path(), "script.sh", b"#!/bin/sh\necho hello\n");
    set_mode(target.path(), "script.sh", 0o755);

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-meta", Some("img-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-meta", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "metadata-only round-trip failed:\n{diffs:#?}"
    );
}

// ── 4. test_roundtrip_symlink ─────────────────────────────────────────────────

/// Symlink target changes — the new target must be recorded and restored.
#[tokio::test]
async fn test_roundtrip_symlink() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(base.path(), "real_file.txt", b"content");
    write_symlink(base.path(), "link", "old_target");

    write_file(target.path(), "real_file.txt", b"content");
    write_symlink(target.path(), "link", "new_target");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-sym", Some("img-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-sym", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(diffs.is_empty(), "symlink round-trip failed:\n{diffs:#?}");
}

// ── 5. test_roundtrip_hardlink ────────────────────────────────────────────────

/// A new hardlink is added in the target — the output must share the same inode.
#[tokio::test]
async fn test_roundtrip_hardlink() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(base.path(), "file_a.txt", b"shared content");
    write_file(target.path(), "file_a.txt", b"shared content");

    // Create file_b as a hardlink to file_a.txt in target.
    std::fs::hard_link(
        target.path().join("file_a.txt"),
        target.path().join("file_b.txt"),
    )
    .unwrap();

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-hl", Some("img-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-hl", base.path()))
        .await
        .unwrap();

    // Both files must exist in output.
    assert!(output.path().join("file_a.txt").exists());
    assert!(output.path().join("file_b.txt").exists());

    // They must share the same inode (hardlink).
    use std::os::unix::fs::MetadataExt;
    let a_ino = std::fs::metadata(output.path().join("file_a.txt"))
        .unwrap()
        .ino();
    let b_ino = std::fs::metadata(output.path().join("file_b.txt"))
        .unwrap()
        .ino();
    assert_eq!(a_ino, b_ino, "file_b should be a hardlink to file_a");
}

// ── 6. test_roundtrip_many_files ──────────────────────────────────────────────

/// 100 files of mixed types — stress test; no panic, output correct.
#[tokio::test]
async fn test_roundtrip_many_files() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // 40 unchanged files
    for i in 0..40 {
        let name = format!("unchanged/file_{i:03}.dat");
        let content = format!("unchanged content {i}");
        write_file(base.path(), &name, content.as_bytes());
        write_file(target.path(), &name, content.as_bytes());
    }
    // 30 changed files
    for i in 0..30 {
        let name = format!("changed/file_{i:03}.dat");
        write_file(base.path(), &name, format!("old content {i}").as_bytes());
        set_mtime_old(base.path(), &name);
        write_file(
            target.path(),
            &name,
            format!("new content {i} updated").as_bytes(),
        );
    }
    // 20 new files
    for i in 0..20 {
        let name = format!("new/file_{i:03}.dat");
        write_file(target.path(), &name, format!("brand new {i}").as_bytes());
    }
    // 10 removed files
    for i in 0..10 {
        let name = format!("removed/file_{i:03}.dat");
        write_file(
            base.path(),
            &name,
            format!("removed content {i}").as_bytes(),
        );
    }

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-many", Some("img-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-many", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "many-files round-trip failed:\n{diffs:#?}"
    );
}

// ── 7. test_compression_stats ─────────────────────────────────────────────────

/// compress() returns non-zero stats for a mixed workload.
#[tokio::test]
async fn test_compression_stats() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();

    write_file(base.path(), "a.txt", b"original content for a");
    set_mtime_old(base.path(), "a.txt");
    write_file(base.path(), "b.txt", b"will be removed");

    write_file(
        target.path(),
        "a.txt",
        b"updated content for a with more text",
    );
    write_file(target.path(), "c.txt", b"new file c");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base").await;

    let stats = compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-stats", Some("img-base")),
        )
        .await
        .unwrap();

    assert!(
        stats.files_patched + stats.files_added + stats.files_removed > 0,
        "expected non-zero stats, got: {stats:?}"
    );
}

// ── 8. test_decompression_stats ───────────────────────────────────────────────

/// patches_verified equals the number of patch entries in the manifest.
#[tokio::test]
async fn test_decompression_stats() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // Two files with enough content that xdelta3 can produce a compact delta.
    let alpha_base: Vec<u8> = b"alpha ".iter().cycle().copied().take(512).collect();
    let mut alpha_target = alpha_base.clone();
    alpha_target[100] = 0xFF;
    alpha_target[200] = 0xAB;

    let beta_base: Vec<u8> = b"beta  ".iter().cycle().copied().take(512).collect();
    let mut beta_target = beta_base.clone();
    beta_target[50] = 0xDD;
    beta_target[300] = 0xCC;

    write_file(base.path(), "alpha.txt", &alpha_base);
    set_mtime_old(base.path(), "alpha.txt");
    write_file(base.path(), "beta.txt", &beta_base);
    set_mtime_old(base.path(), "beta.txt");

    write_file(target.path(), "alpha.txt", &alpha_target);
    write_file(target.path(), "beta.txt", &beta_target);

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-decomp-stats", Some("img-base")),
        )
        .await
        .unwrap();

    let decomp_stats = compressor
        .decompress(
            output.path(),
            decompress_opts("img-decomp-stats", base.path()),
        )
        .await
        .unwrap();

    // Both files were patched → patches_verified should be 2.
    assert_eq!(
        decomp_stats.patches_verified, 2,
        "expected 2 patches_verified, got {decomp_stats:?}"
    );
}

// ── 9. test_parallel_same_result_as_sequential ────────────────────────────────

/// Compressing with workers=4 must produce the same decompressed output as workers=1.
/// This verifies rayon parallelism is correct and deterministic for content.
#[tokio::test]
async fn test_parallel_same_result_as_sequential() {
    use common::compress_opts_workers;

    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let out_seq = tempdir().unwrap();
    let out_par = tempdir().unwrap();

    // 8 changed files so rayon has something to parallelize
    for i in 0..8 {
        let name = format!("file_{i:02}.dat");
        let old: Vec<u8> = format!("old content for file {i} --- padding")
            .bytes()
            .cycle()
            .take(256)
            .collect();
        let mut new = old.clone();
        new[i * 10] ^= 0xAB;
        write_file(base.path(), &name, &old);
        set_mtime_old(base.path(), &name);
        write_file(target.path(), &name, &new);
    }

    // Sequential (workers=1)
    {
        let (storage, compressor) = make_compressor();
        save_root_meta(&*storage, "img-base-seq").await;
        compressor
            .compress(
                base.path(),
                target.path(),
                compress_opts_workers("img-seq", Some("img-base-seq"), 1),
            )
            .await
            .unwrap();
        compressor
            .decompress(out_seq.path(), decompress_opts("img-seq", base.path()))
            .await
            .unwrap();
    }

    // Parallel (workers=4)
    {
        let (storage, compressor) = make_compressor();
        save_root_meta(&*storage, "img-base-par").await;
        compressor
            .compress(
                base.path(),
                target.path(),
                compress_opts_workers("img-par", Some("img-base-par"), 4),
            )
            .await
            .unwrap();
        compressor
            .decompress(out_par.path(), decompress_opts("img-par", base.path()))
            .await
            .unwrap();
    }

    let diffs = compare_dirs(out_seq.path(), out_par.path());
    assert!(
        diffs.is_empty(),
        "parallel output differs from sequential:\n{diffs:#?}"
    );
}

// ── 10. test_roundtrip_first_image ────────────────────────────────────────────

/// No base image — all files are new (blob additions, `old_path = None`).
///
/// Exercises the path where:
/// - `copy_unchanged_from_base` walks an empty dir → produces nothing
/// - Every record goes through `Data::BlobRef` download
/// - The patches tar exists but has zero entries
#[tokio::test]
async fn test_roundtrip_first_image() {
    let base = tempdir().unwrap(); // empty — first image has no base
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(target.path(), "etc/hosts", b"127.0.0.1 localhost\n");
    write_file(target.path(), "etc/passwd", b"root:x:0:0::/root:/bin/sh\n");
    write_file(
        target.path(),
        "usr/bin/ls",
        b"\x7fELF placeholder binary\x00\x01\x02",
    );
    write_symlink(target.path(), "bin", "usr/bin");

    let (_, compressor) = make_compressor();

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-first", None), // no base
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-first", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "first-image round-trip failed:\n{diffs:#?}"
    );
}

// ── 11. test_roundtrip_chain_v0_v1_v2 ────────────────────────────────────────

/// Three-version chain: v0 (first image) → v1 (delta) → v2 (delta).
///
/// This is the most important end-to-end scenario for the whole system.
///
/// Key correctness invariant tested by phase D:
///   **The output of decompressing v1 must faithfully serve as the base for
///   decompressing v2.**
///
/// `set_mtime_old` strategy:
/// - Called on v0 files that *change* in v1 **before** compress v0→v1 so the
///   diff sees a mtime delta and records `metadata.mtime` in the manifest.
/// - Called on v1 files that *change* in v2 **after** the phase-B assertion
///   (so that assertion is unaffected) and **before** compress v1→v2.
/// - Unchanged files are left with fresh mtimes so `copy_unchanged_from_base`
///   preserves source mtime and both sides stay within the 1-second tolerance.
#[tokio::test]
async fn test_roundtrip_chain_v0_v1_v2() {
    let v0 = tempdir().unwrap();
    let v1 = tempdir().unwrap();
    let v2 = tempdir().unwrap();

    // v0: initial version
    write_file(
        v0.path(),
        "kernel",
        b"kernel v0 --- padding --- padding ---",
    );
    write_file(
        v0.path(),
        "libc.so",
        b"common lib bytes unchanged across versions",
    );
    write_file(v0.path(), "etc/hosts", b"127.0.0.1 localhost v0");
    write_file(v0.path(), "etc/os-release", b"VERSION=0");

    // v1: kernel updated, etc/hosts updated, os-release removed, newcmd added
    write_file(v1.path(), "kernel", b"kernel v1 updated --- padding ---");
    write_file(
        v1.path(),
        "libc.so",
        b"common lib bytes unchanged across versions",
    );
    write_file(v1.path(), "etc/hosts", b"127.0.0.1 localhost v1 updated");
    write_file(v1.path(), "usr/bin/newcmd", b"brand new binary in v1");
    // etc/os-release removed in v1

    // v2: kernel and libc updated, etc/hosts unchanged (same as v1), tmp added
    write_file(v2.path(), "kernel", b"kernel v2 latest --- padding ---");
    write_file(v2.path(), "libc.so", b"libc updated to v2 with fixes");
    write_file(v2.path(), "etc/hosts", b"127.0.0.1 localhost v1 updated"); // same as v1
    write_file(v2.path(), "usr/bin/newcmd", b"brand new binary in v1"); // same as v1
    write_file(v2.path(), "tmp/run.log", b"new log file in v2");

    let empty_base = tempdir().unwrap();
    let (storage, compressor) = make_compressor();

    // ── Phase A: compress v0 (first image, no base) ───────────────────────────
    // Mark v0 base files that will change in v1 as old so the v0→v1 diff
    // detects a mtime change and records metadata.mtime in the manifest.
    // libc.so is unchanged in v1 — no set_mtime_old needed.
    set_mtime_old(v0.path(), "kernel");
    set_mtime_old(v0.path(), "etc/hosts");

    compressor
        .compress(
            empty_base.path(),
            v0.path(),
            compress_opts("chain-v0", None),
        )
        .await
        .unwrap();

    let reconstruct_v0 = tempdir().unwrap();
    compressor
        .decompress(
            reconstruct_v0.path(),
            decompress_opts("chain-v0", empty_base.path()),
        )
        .await
        .unwrap();

    let diffs = compare_dirs(v0.path(), reconstruct_v0.path());
    assert!(diffs.is_empty(), "chain v0 round-trip failed:\n{diffs:#?}");

    // ── Phase B: compress v0→v1, decompress v1 ────────────────────────────────
    // v0/kernel is old, v1/kernel is fresh → diff records mtime change ✓
    save_root_meta(&*storage, "chain-v0").await;
    compressor
        .compress(
            v0.path(),
            v1.path(),
            compress_opts("chain-v1", Some("chain-v0")),
        )
        .await
        .unwrap();

    let reconstruct_v1 = tempdir().unwrap();
    compressor
        .decompress(
            reconstruct_v1.path(),
            decompress_opts("chain-v1", v0.path()),
        )
        .await
        .unwrap();

    let diffs = compare_dirs(v1.path(), reconstruct_v1.path());
    assert!(diffs.is_empty(), "chain v1 round-trip failed:\n{diffs:#?}");

    // ── Phase C: mark v1 files old (for v1→v2 diff) ──────────────────────────
    // Done AFTER the phase-B assertion so compare_dirs(v1, reconstruct_v1) was
    // not affected.  kernel and libc.so both change in v2.
    set_mtime_old(v1.path(), "kernel");
    set_mtime_old(v1.path(), "libc.so");

    // ── Phase D: compress v1→v2, decompress v2 using RECONSTRUCTED v1 ─────────
    // Key step: reconstruct_v1 (not original v1) is used as the decompress
    // base, verifying that decompressed output faithfully serves as the next
    // base in the chain.
    save_root_meta(&*storage, "chain-v1").await;
    compressor
        .compress(
            v1.path(),
            v2.path(),
            compress_opts("chain-v2", Some("chain-v1")),
        )
        .await
        .unwrap();

    let reconstruct_v2 = tempdir().unwrap();
    compressor
        .decompress(
            reconstruct_v2.path(),
            decompress_opts("chain-v2", reconstruct_v1.path()),
        )
        .await
        .unwrap();

    let diffs = compare_dirs(v2.path(), reconstruct_v2.path());
    assert!(
        diffs.is_empty(),
        "chain v2 round-trip (using decompressed v1 as base) failed:\n{diffs:#?}"
    );
}

// ── 12. test_roundtrip_all_deletions ─────────────────────────────────────────

/// All base files are deleted in the target — the output must be empty.
///
/// Exercises:
/// - `affected` set = all old_paths → `copy_unchanged_from_base` copies nothing
/// - All records have `new_path = None` → `apply_record` is a no-op for each
#[tokio::test]
async fn test_roundtrip_all_deletions() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap(); // empty target = all base files deleted
    let output = tempdir().unwrap();

    write_file(base.path(), "file_a.txt", b"content a");
    write_file(base.path(), "file_b.txt", b"content b");
    write_file(base.path(), "subdir/file_c.txt", b"content c");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-all-del-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-all-del", Some("img-all-del-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-all-del", base.path()))
        .await
        .unwrap();

    // Output must be empty (no files surviving from base or added in target)
    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "all-deletions round-trip failed:\n{diffs:#?}"
    );
}

// ── 13. test_roundtrip_new_symlink ────────────────────────────────────────────

/// A symlink is ADDED in the target (no corresponding symlink in base).
///
/// This exercises the `EntryType::Symlink` + `Data::SoftlinkTo` path in
/// `apply_record`, which is distinct from the changed-symlink path
/// (`Patch::Real`) exercised by `test_roundtrip_symlink`.
#[tokio::test]
async fn test_roundtrip_new_symlink() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(base.path(), "real_file.txt", b"content");
    write_file(target.path(), "real_file.txt", b"content");
    write_symlink(target.path(), "link_to_real", "real_file.txt");
    write_symlink(target.path(), "deep/link", "../real_file.txt");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-new-sym-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-new-sym", Some("img-new-sym-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-new-sym", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "new-symlink round-trip failed:\n{diffs:#?}"
    );
}

// ── 14. test_roundtrip_unchanged_symlink_in_base ──────────────────────────────

/// A symlink exists in both base and target with the same target string.
///
/// It is NOT modified → no record is generated for it → it should be copied
/// from base by `copy_unchanged_from_base` (the symlink branch).
#[tokio::test]
async fn test_roundtrip_unchanged_symlink_in_base() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // Unchanged symlink: same target in both base and target
    write_file(base.path(), "real_file.txt", b"content");
    write_symlink(base.path(), "link", "real_file.txt");

    write_file(target.path(), "real_file.txt", b"content updated");
    set_mtime_old(base.path(), "real_file.txt");
    write_symlink(target.path(), "link", "real_file.txt"); // same target → no record

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-unch-sym-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-unch-sym", Some("img-unch-sym-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-unch-sym", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "unchanged-symlink round-trip failed:\n{diffs:#?}"
    );
}

// ── 15. test_roundtrip_rename_and_change ─────────────────────────────────────

/// A file is renamed AND its content changes in the same version.
///
/// This exercises the `old_path ≠ new_path` + `Patch::Real` combined path,
/// unlike `test_roundtrip_rename` (same content) and `test_roundtrip_simple`
/// (same path, changed content).
#[tokio::test]
async fn test_roundtrip_rename_and_change() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // library renamed from v1 to v2 AND content updated
    write_file(
        base.path(),
        "lib/libfoo-1.0.so",
        b"ELF library version 1.0 data placeholder bytes",
    );
    write_file(base.path(), "unchanged.txt", b"this file stays");
    set_mtime_old(base.path(), "lib/libfoo-1.0.so");

    write_file(
        target.path(),
        "lib/libfoo-2.0.so",
        b"ELF library version 2.0 data placeholder bytes updated",
    );
    write_file(target.path(), "unchanged.txt", b"this file stays");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-rnc-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-rnc", Some("img-rnc-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-rnc", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "rename-and-change round-trip failed:\n{diffs:#?}"
    );
}

// ── 16. test_roundtrip_compressed_patches ────────────────────────────────────

/// Force `patches_compressed = true` by using large repetitive content.
///
/// The `try_gzip` function uses gzip only when it makes the archive smaller.
/// Repetitive binary content compresses well, so this test exercises the
/// gzip code path in both compress (archive creation) and decompress
/// (archive extraction with `patches_compressed = true`).
#[tokio::test]
async fn test_roundtrip_compressed_patches() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // Large repetitive content: gzip will shrink this significantly
    let base_content: Vec<u8> = b"AAAAAAAABBBBBBBBCCCCCCCCDDDDDDDD"
        .iter()
        .cycle()
        .copied()
        .take(32 * 1024)
        .collect();
    let mut target_content = base_content.clone();
    // Modify a few bytes in the middle
    target_content[8000] = 0xFF;
    target_content[16000] = 0xAB;
    target_content[24000] = 0x42;

    write_file(base.path(), "big_file.bin", &base_content);
    set_mtime_old(base.path(), "big_file.bin");
    write_file(target.path(), "big_file.bin", &target_content);

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-gzip-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-gzip", Some("img-gzip-base")),
        )
        .await
        .unwrap();

    // Verify that the patches archive was actually gzip-compressed
    // (only if content is compressible enough; PassthroughEncoder stores full
    // target content as patch, 32 KB of AAAAAA... compresses to ~100 bytes)
    assert_eq!(
        storage.patches_were_compressed("img-gzip"),
        Some(true),
        "expected gzip compression for highly repetitive content"
    );

    compressor
        .decompress(output.path(), decompress_opts("img-gzip", base.path()))
        .await
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "compressed-patches round-trip failed:\n{diffs:#?}"
    );
}
