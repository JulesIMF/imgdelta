mod common;

use common::{compress_opts, decompress_opts, make_compressor, set_mtime_old, write_file};
use image_delta_core::{Compressor, ImageMeta, Storage};
use tempfile::tempdir;
use uuid::Uuid;

async fn save_root_meta(storage: &dyn Storage, image_id: &str) {
    storage
        .register_image(&ImageMeta {
            image_id: image_id.to_string(),
            base_image_id: None,
            format: "directory".into(),
        })
        .await
        .unwrap();
}

// ── 1. test_blob_patch_detected ───────────────────────────────────────────────

/// When a new file is similar to a blob from the base image (by path),
/// the manifest entry should use BlobPatch (blob + patch) rather than a plain blob.
///
/// We set up FakeStorage with a pre-existing blob and register it as an origin
/// so that `find_blob_candidates` returns it.  Then we check that the manifest
/// entry for the new file has both `blob` and `patch` set.
#[tokio::test]
async fn test_blob_patch_detected() {
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
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-bp", Some("img-base")),
        )
        .await
        .unwrap();

    // Inspect the manifest: the changed file should be stored as a patch.
    let manifest_bytes = storage.download_manifest("img-bp").await.unwrap();
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
#[tokio::test]
async fn test_blob_patch_result_correct() {
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
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-bpr", Some("img-base")),
        )
        .await
        .unwrap();

    compressor
        .decompress(output.path(), decompress_opts("img-bpr", base.path()))
        .await
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
#[tokio::test]
async fn test_blob_patch_fallback_to_blob() {
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
    let unrelated_data = b"unrelated blob";
    let unrelated_sha = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(unrelated_data))
    };
    let unrelated_blob_id: Uuid = storage
        .upload_blob(&unrelated_sha, unrelated_data)
        .await
        .unwrap();
    storage.register_blob_origin("img-base", unrelated_blob_id, "something/unrelated.txt");
    save_root_meta(&*storage, "img-base").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-fallback", Some("img-base")),
        )
        .await
        .unwrap();

    // Check the manifest: totally_new.txt should be a plain blob (no patch).
    let manifest_bytes = storage.download_manifest("img-fallback").await.unwrap();
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
        .await
        .unwrap();

    let output_bytes = std::fs::read(output.path().join("totally_new.txt")).unwrap();
    assert_eq!(output_bytes, b"completely unrelated content xyz");
}

// ── 4. test_record_blob_origin_stored ────────────────────────────────────────

/// Compressing an image must call `record_blob_origin` for each newly uploaded blob.
/// After compression, `find_blob_candidates(image_id)` must return those blobs.
#[tokio::test]
async fn test_record_blob_origin_stored() {
    let base = tempdir().unwrap();
    let target = tempdir().unwrap();

    // Both dirs have a common unchanged file.
    write_file(base.path(), "common.txt", b"shared");
    write_file(target.path(), "common.txt", b"shared");

    // Target adds two new files.
    write_file(target.path(), "lib/libfoo.so.1", b"library v1 content xxxx");
    write_file(target.path(), "data/config.bin", b"configuration data yyyy");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-root").await;

    compressor
        .compress(
            base.path(),
            target.path(),
            compress_opts("img-with-blobs", Some("img-root")),
        )
        .await
        .unwrap();

    // find_blob_candidates for the image we just produced should return the two new blobs.
    let candidates = storage
        .find_blob_candidates("img-with-blobs")
        .await
        .unwrap();
    let candidate_paths: Vec<&str> = candidates
        .iter()
        .map(|c| c.original_path.as_str())
        .collect();

    assert!(
        candidate_paths.contains(&"lib/libfoo.so.1"),
        "lib/libfoo.so.1 blob must be recorded; got: {candidate_paths:?}"
    );
    assert!(
        candidate_paths.contains(&"data/config.bin"),
        "data/config.bin blob must be recorded; got: {candidate_paths:?}"
    );
}

// ── 5. test_cross_image_blobpatch_detected ───────────────────────────────────

/// A newly added file that is structurally similar to a blob from the previous image
/// (matched by path-similarity) must be encoded as a BlobPatch (blob + patch) rather
/// than a plain full blob.
///
/// Scenario:
///   Round 1: compress base → v1, adding `lib/libfoo.so.1` as a new blob (origin recorded).
///   Round 2: compress v1 → v2, adding `lib/libfoo.so.2` (similar to libfoo.so.1).
///   Expected: manifest entry for libfoo.so.2 has both `blob` (base) and `patch` (delta).
#[tokio::test]
async fn test_cross_image_blobpatch_detected() {
    use image_delta_core::manifest::Manifest;

    let root = tempdir().unwrap(); // empty "root" image
    let v1 = tempdir().unwrap();
    let v2 = tempdir().unwrap();

    // v1 has a shared file and libfoo.so.1.
    let lib_v1: Vec<u8> = (0u32..512).flat_map(|i| i.to_le_bytes()).collect(); // 2048 bytes
    write_file(v1.path(), "readme.txt", b"readme");
    write_file(v1.path(), "lib/libfoo.so.1", &lib_v1);

    // v2 keeps readme and libfoo.so.1 unchanged, adds libfoo.so.2 (similar bytes).
    let mut lib_v2 = lib_v1.clone();
    lib_v2[256] = 0xFF;
    lib_v2[512] = 0xAB;
    lib_v2[768] = 0x12;
    write_file(v2.path(), "readme.txt", b"readme");
    write_file(v2.path(), "lib/libfoo.so.1", &lib_v1);
    write_file(v2.path(), "lib/libfoo.so.2", &lib_v2);

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-root").await;

    // Round 1: compress root → v1. This records libfoo.so.1 as a blob origin.
    compressor
        .compress(
            root.path(),
            v1.path(),
            compress_opts("img-v1", Some("img-root")),
        )
        .await
        .unwrap();

    // Round 2: compress v1 → v2.
    compressor
        .compress(
            v1.path(),
            v2.path(),
            compress_opts("img-v2", Some("img-v1")),
        )
        .await
        .unwrap();

    // Inspect the manifest for img-v2: libfoo.so.2 must be a BlobPatch.
    let manifest_bytes = storage.download_manifest("img-v2").await.unwrap();
    let manifest: Manifest = rmp_serde::from_slice(&manifest_bytes).unwrap();

    let entry = manifest
        .entries
        .iter()
        .find(|e| e.path == "lib/libfoo.so.2")
        .expect("lib/libfoo.so.2 must appear in the v2 manifest");

    assert!(
        entry.blob.is_some(),
        "BlobPatch entry must reference the base blob; entry: {entry:?}"
    );
    assert!(
        entry.patch.is_some(),
        "BlobPatch entry must have a patch; entry: {entry:?}"
    );
}

// ── 6. test_cross_image_blobpatch_roundtrip ──────────────────────────────────

/// End-to-end round-trip for the cross-image BlobPatch path:
/// compress two rounds then decompress and verify the output byte-for-byte.
#[tokio::test]
async fn test_cross_image_blobpatch_roundtrip() {
    let root = tempdir().unwrap();
    let v1 = tempdir().unwrap();
    let v2 = tempdir().unwrap();
    let output = tempdir().unwrap();

    let lib_v1: Vec<u8> = (0u32..512).flat_map(|i| i.to_le_bytes()).collect();
    let mut lib_v2 = lib_v1.clone();
    lib_v2[100] = 0xDE;
    lib_v2[400] = 0xAD;

    write_file(v1.path(), "lib/libfoo.so.1", &lib_v1);
    write_file(v1.path(), "other.txt", b"unchanged file");

    write_file(v2.path(), "lib/libfoo.so.1", &lib_v1);
    write_file(v2.path(), "lib/libfoo.so.2", &lib_v2);
    write_file(v2.path(), "other.txt", b"unchanged file");

    let (storage, compressor) = make_compressor();
    save_root_meta(&*storage, "img-root").await;

    compressor
        .compress(
            root.path(),
            v1.path(),
            compress_opts("img-v1", Some("img-root")),
        )
        .await
        .unwrap();

    compressor
        .compress(
            v1.path(),
            v2.path(),
            compress_opts("img-v2", Some("img-v1")),
        )
        .await
        .unwrap();

    // Decompress img-v2 using the physical v1 directory as base.
    // We override img-v1's metadata to have no base_image_id so the
    // chain-detection guard (which protects against un-decompressed intermediate
    // deltas) does not fire — we already have v1 as a physical directory.
    storage
        .register_image(&ImageMeta {
            image_id: "img-v1".to_string(),
            base_image_id: None,
            format: "directory".into(),
        })
        .await
        .unwrap();
    compressor
        .decompress(output.path(), decompress_opts("img-v2", v1.path()))
        .await
        .unwrap();

    // Output must exactly match v2.
    let got_v2 = std::fs::read(output.path().join("lib/libfoo.so.2")).unwrap();
    assert_eq!(
        got_v2, lib_v2,
        "lib/libfoo.so.2 content mismatch after BlobPatch round-trip"
    );

    let got_other = std::fs::read(output.path().join("other.txt")).unwrap();
    assert_eq!(got_other, b"unchanged file");
}
