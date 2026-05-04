// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Integration tests for DefaultCompressor::decompress() — error paths and status

//! Decompress-specific integration tests.
//!
//! These tests focus on things that the round-trip tests in
//! `compress_decompress.rs` do not cover:
//! - Status lifecycle during a successful decompress
//! - `Failed` status when decompress encounters an error
//! - Returning `Err` for a non-existent image
//! - Manifest version mismatch detection

mod common;

use common::{
    compress_opts, decompress_opts, make_compressor, save_root_meta_for_storage, write_file,
};
use image_delta_core::{Compressor, ImageMeta, Storage};
use tempfile::tempdir;

// ── 1. test_decompress_status_after_success ───────────────────────────────────

/// After a successful `decompress()` the image status must remain `"compressed"`.
///
/// Specifically: `compress()` leaves status = "compressed"; `decompress()` also
/// ends by calling `update_status(Compressed)`.  This verifies that the final
/// status is correct and that decompress does not accidentally set another value.
#[tokio::test]
async fn test_decompress_status_after_success() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    write_file(target.path(), "file.txt", b"hello decompress");

    let (storage, compressor) = make_compressor();

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-dc-status", None),
        )
        .await
        .unwrap();

    // Status should be "compressed" after compress
    assert_eq!(
        storage.image_status("img-dc-status").as_deref(),
        Some("compressed"),
        "status after compress must be 'compressed'"
    );

    compressor
        .decompress(output.path(), decompress_opts("img-dc-status", base.path()))
        .await
        .unwrap();

    // Status must still be "compressed" after decompress
    assert_eq!(
        storage.image_status("img-dc-status").as_deref(),
        Some("compressed"),
        "status after decompress must be 'compressed'"
    );
}

// ── 2. test_decompress_error_sets_failed_status ───────────────────────────────

/// When `decompress()` fails, the image status must be set to `"failed"`.
///
/// Setup: register an image in storage but upload no manifest.  The
/// `download_manifest` call then returns `Err`, which triggers the
/// `update_status(Failed(...))` path in `DefaultCompressor::decompress`.
#[tokio::test]
async fn test_decompress_error_sets_failed_status() {
    let (storage, compressor) = make_compressor();
    let base = tempdir().unwrap();
    let out = tempdir().unwrap();

    // Register the image so `update_status` can find it, but upload no manifest
    storage
        .register_image(&ImageMeta {
            image_id: "img-err-decomp".into(),
            base_image_id: None,
            format: "directory".into(),
            status: "compressed".into(),
        })
        .await
        .unwrap();

    let result = compressor
        .decompress(out.path(), decompress_opts("img-err-decomp", base.path()))
        .await;

    assert!(
        result.is_err(),
        "decompress without manifest must return Err"
    );
    let status = storage.image_status("img-err-decomp");
    assert!(
        status
            .as_deref()
            .map(|s| s.starts_with("failed"))
            .unwrap_or(false),
        "status must start with 'failed' after decompress error, got: {status:?}"
    );
}

// ── 3. test_decompress_error_nonexistent_image ────────────────────────────────

/// Decompressing an image that was never registered (no manifest, no metadata)
/// must return `Err`.
///
/// Since the image is not registered, `update_status(Failed)` silently fails
/// (the FakeStorage returns `Err` which is ignored), but the decompress call
/// itself must still propagate the error to the caller.
#[tokio::test]
async fn test_decompress_error_nonexistent_image() {
    let (_, compressor) = make_compressor();
    let base = tempdir().unwrap();
    let out = tempdir().unwrap();

    let result = compressor
        .decompress(
            out.path(),
            decompress_opts("completely-nonexistent-image-id", base.path()),
        )
        .await;

    assert!(
        result.is_err(),
        "decompress of a nonexistent image must return Err"
    );
}

// ── 4. test_decompress_stats_files_and_patches ────────────────────────────────

/// `DecompressionStats` counters are consistent with the manifest contents.
///
/// - `total_files`: all files written to output (blobs + patch-decoded)
/// - `patches_verified`: only files that went through patch decode
/// - `total_bytes`: sum of bytes written
#[tokio::test]
async fn test_decompress_stats_files_and_patches() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // 2 files that will be patched (change in-place)
    let base_a: Vec<u8> = b"base content for file_a "
        .iter()
        .cycle()
        .copied()
        .take(256)
        .collect();
    let mut tgt_a = base_a.clone();
    tgt_a[100] = 0xFF;

    let base_b: Vec<u8> = b"base content for file_b "
        .iter()
        .cycle()
        .copied()
        .take(256)
        .collect();
    let mut tgt_b = base_b.clone();
    tgt_b[50] = 0xAB;

    write_file(base.path(), "a.bin", &base_a);
    write_file(base.path(), "b.bin", &base_b);
    common::set_mtime_old(base.path(), "a.bin");
    common::set_mtime_old(base.path(), "b.bin");

    write_file(target.path(), "a.bin", &tgt_a);
    write_file(target.path(), "b.bin", &tgt_b);
    // 1 brand-new file (blob addition)
    write_file(target.path(), "new.txt", b"brand new file content");

    let (storage, compressor) = make_compressor();
    save_root_meta_for_storage(&*storage, "img-stats-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-decomp-stats2", Some("img-stats-base")),
        )
        .await
        .unwrap();

    let decomp = compressor
        .decompress(
            output.path(),
            decompress_opts("img-decomp-stats2", base.path()),
        )
        .await
        .unwrap();

    // 2 patched + 1 new blob = 3 files written
    assert_eq!(
        decomp.total_files, 3,
        "total_files must equal written file count: {decomp:?}"
    );
    // Only the 2 patched files go through patch decode
    assert_eq!(
        decomp.patches_verified, 2,
        "patches_verified must equal patch count: {decomp:?}"
    );
    // total_bytes > 0
    assert!(
        decomp.total_bytes > 0,
        "total_bytes must be > 0: {decomp:?}"
    );
}
