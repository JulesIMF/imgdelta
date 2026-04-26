mod common;

use common::{
    compare_dirs, compress_opts, decompress_opts, make_compressor, set_mode, set_mtime_old,
    write_file, write_symlink,
};
use image_delta_core::{Compressor, ImageMeta, Storage};
use tempfile::tempdir;

// ── Helper: save root image meta so chain-check passes ────────────────────────

fn save_root_meta(storage: &dyn Storage, image_id: &str) {
    storage
        .save_image_meta(&ImageMeta {
            image_id: image_id.to_string(),
            base_image_id: None,
            format: "directory".into(),
        })
        .unwrap();
}

// ── 1. test_roundtrip_simple ──────────────────────────────────────────────────

/// base = {file_a, file_b, file_c}; target = {file_a changed, file_c unchanged,
/// file_d new, file_b removed}.  After compress + decompress, output == target.
#[test]
fn test_roundtrip_simple() {
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
    save_root_meta(&*storage, "img-base");

    let stats = compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-1", Some("img-base")),
        )
        .unwrap();

    let decomp = compressor
        .decompress(output.path(), decompress_opts("img-1", base.path()))
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
#[test]
fn test_roundtrip_rename() {
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
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-rename", Some("img-base")),
        )
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-rename", base.path()))
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(diffs.is_empty(), "rename round-trip failed:\n{diffs:#?}");
}

// ── 3. test_roundtrip_metadata_only ──────────────────────────────────────────

/// Only the mode changes — no content diff.
#[test]
fn test_roundtrip_metadata_only() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(base.path(), "script.sh", b"#!/bin/sh\necho hello\n");
    set_mode(base.path(), "script.sh", 0o644);

    write_file(target.path(), "script.sh", b"#!/bin/sh\necho hello\n");
    set_mode(target.path(), "script.sh", 0o755);

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-meta", Some("img-base")),
        )
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-meta", base.path()))
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "metadata-only round-trip failed:\n{diffs:#?}"
    );
}

// ── 4. test_roundtrip_symlink ─────────────────────────────────────────────────

/// Symlink target changes — the new target must be recorded and restored.
#[test]
fn test_roundtrip_symlink() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(base.path(), "real_file.txt", b"content");
    write_symlink(base.path(), "link", "old_target");

    write_file(target.path(), "real_file.txt", b"content");
    write_symlink(target.path(), "link", "new_target");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-sym", Some("img-base")),
        )
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-sym", base.path()))
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(diffs.is_empty(), "symlink round-trip failed:\n{diffs:#?}");
}

// ── 5. test_roundtrip_hardlink ────────────────────────────────────────────────

/// A new hardlink is added in the target — the output must share the same inode.
#[test]
fn test_roundtrip_hardlink() {
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
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-hl", Some("img-base")),
        )
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-hl", base.path()))
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
#[test]
fn test_roundtrip_many_files() {
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
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-many", Some("img-base")),
        )
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-many", base.path()))
        .unwrap();

    let diffs = compare_dirs(target.path(), output.path());
    assert!(
        diffs.is_empty(),
        "many-files round-trip failed:\n{diffs:#?}"
    );
}

// ── 7. test_compression_stats ─────────────────────────────────────────────────

/// compress() returns non-zero stats for a mixed workload.
#[test]
fn test_compression_stats() {
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
    save_root_meta(&*storage, "img-base");

    let stats = compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-stats", Some("img-base")),
        )
        .unwrap();

    assert!(
        stats.files_patched + stats.files_added + stats.files_removed > 0,
        "expected non-zero stats, got: {stats:?}"
    );
}

// ── 8. test_decompression_stats ───────────────────────────────────────────────

/// patches_verified equals the number of patch entries in the manifest.
#[test]
fn test_decompression_stats() {
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
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-decomp-stats", Some("img-base")),
        )
        .unwrap();

    let decomp_stats = compressor
        .decompress(
            output.path(),
            decompress_opts("img-decomp-stats", base.path()),
        )
        .unwrap();

    // Both files were patched → patches_verified should be 2.
    assert_eq!(
        decomp_stats.patches_verified, 2,
        "expected 2 patches_verified, got {decomp_stats:?}"
    );
}
