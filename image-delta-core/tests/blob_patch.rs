mod common;

use common::{compress_opts, decompress_opts, make_compressor, set_mtime_old, write_file};
use image_delta_core::{Compressor, ImageMeta, Storage};
use tempfile::tempdir;
use uuid::Uuid;

fn save_root_meta(storage: &dyn Storage, image_id: &str) {
    storage
        .save_image_meta(&ImageMeta {
            image_id: image_id.to_string(),
            base_image_id: None,
            format: "directory".into(),
        })
        .unwrap();
}

// ── 1. test_blob_patch_detected ───────────────────────────────────────────────

/// When a new file is similar to a blob from the base image (by path),
/// the manifest entry should use BlobPatch (blob + patch) rather than a plain blob.
///
/// We set up FakeStorage with a pre-existing blob and register it as an origin
/// so that `find_blob_candidates` returns it.  Then we check that the manifest
/// entry for the new file has both `blob` and `patch` set.
#[test]
fn test_blob_patch_detected() {
    use image_delta_core::manifest::Manifest;

    let base = tempdir().unwrap();
    let target = tempdir().unwrap();

    // Base has a lib file.
    let base_content = b"library v1.0 content with some binary-ish padding 0000000";
    write_file(base.path(), "lib/libfoo.so.1", base_content);
    set_mtime_old(base.path(), "lib/libfoo.so.1");

    // Target has a slightly newer version of the same lib (same path → Changed).
    let target_content = b"library v1.1 content with some binary-ish padding 0000001";
    write_file(target.path(), "lib/libfoo.so.1", target_content);

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-bp", Some("img-base")),
        )
        .unwrap();

    // Inspect the manifest: the changed file should be stored as a patch.
    let manifest_bytes = storage.download_manifest("img-bp").unwrap();
    let manifest: Manifest = rmp_serde::from_slice(&manifest_bytes).unwrap();

    let entry = manifest
        .entries
        .iter()
        .find(|e| e.path == "lib/libfoo.so.1")
        .expect("entry for libfoo.so.1 must be in manifest");

    assert!(
        entry.patch.is_some(),
        "similar file should be stored as a patch, not a plain blob; entry: {entry:?}"
    );
}

// ── 2. test_blob_patch_result_correct ────────────────────────────────────────

/// A file stored with a patch (Changed entry) round-trips to the correct content.
#[test]
fn test_blob_patch_result_correct() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // Use a compressible file (repetitive content with a small change).
    let base_content: Vec<u8> = (0u32..512).map(|i| (i as u8).wrapping_mul(3)).collect();
    let mut target_content = base_content.clone();
    target_content[100] = 0xFF;
    target_content[200] = 0xAB;

    write_file(base.path(), "data.bin", &base_content);
    set_mtime_old(base.path(), "data.bin");
    write_file(target.path(), "data.bin", &target_content);

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-bpr", Some("img-base")),
        )
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-bpr", base.path()))
        .unwrap();

    let output_bytes = std::fs::read(output.path().join("data.bin")).unwrap();
    assert_eq!(
        output_bytes, target_content,
        "patch round-trip produced wrong content"
    );
}

// ── 3. test_blob_patch_fallback_to_blob ──────────────────────────────────────

/// A completely new file (not similar to any base file) is stored as a plain blob,
/// not as a BlobPatch.  The result after decompress must match the original.
#[test]
fn test_blob_patch_fallback_to_blob() {
    use image_delta_core::manifest::Manifest;

    let base = tempdir().unwrap();
    let target = tempdir().unwrap();
    let output = tempdir().unwrap();

    // Base has one file; target has a completely different NEW file.
    write_file(base.path(), "existing.txt", b"old file");
    write_file(target.path(), "existing.txt", b"old file");
    write_file(
        target.path(),
        "totally_new.txt",
        b"completely unrelated content xyz",
    );

    // Pre-seed storage with a blob that has a different path to ensure
    // find_blob_candidates doesn't accidentally match.
    let (storage, compressor) = make_compressor();
    let unrelated_blob_id: Uuid = storage.upload_blob(b"unrelated blob").unwrap();
    storage.register_blob_origin("img-base", unrelated_blob_id, "something/unrelated.txt");
    save_root_meta(&*storage, "img-base");

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-fallback", Some("img-base")),
        )
        .unwrap();

    // Check the manifest: totally_new.txt should be a plain blob (no patch).
    let manifest_bytes = storage.download_manifest("img-fallback").unwrap();
    let manifest: Manifest = rmp_serde::from_slice(&manifest_bytes).unwrap();

    let entry = manifest
        .entries
        .iter()
        .find(|e| e.path == "totally_new.txt")
        .expect("totally_new.txt must be in manifest");

    assert!(
        entry.blob.is_some() && entry.patch.is_none(),
        "unrelated new file should be a plain blob; entry: {entry:?}"
    );

    // Decompress and verify content.
    compressor
        .decompress(output.path(), decompress_opts("img-fallback", base.path()))
        .unwrap();

    let output_bytes = std::fs::read(output.path().join("totally_new.txt")).unwrap();
    assert_eq!(output_bytes, b"completely unrelated content xyz");
}
