mod common;

use common::{compress_opts, decompress_opts, make_compressor, write_file};
use image_delta_core::{Compressor, ImageMeta, Storage};
use tempfile::tempdir;

fn save_meta_with_base(storage: &dyn Storage, image_id: &str, base_image_id: Option<&str>) {
    storage
        .register_image(&ImageMeta {
            image_id: image_id.to_string(),
            base_image_id: base_image_id.map(|s| s.to_string()),
            format: "directory".into(),
        })
        .unwrap();
}

// ── test_chain_not_supported ──────────────────────────────────────────────────

/// Attempting to decompress an image whose base is itself a delta (i.e. forming
/// a chain) must return an error with a message containing "chained
/// decompression".
///
/// Setup:
///   img-root: base_image_id = None          (full image)
///   img-1:    base_image_id = Some(img-root) (delta level 1)
///   img-2:    base_image_id = Some(img-1)    (delta level 2 — chain!)
///
/// Compressing img-2 → img-1 succeeds (base is a delta; that's fine for
/// compress).  Decompressing img-2 must fail because img-1 is itself a delta.
#[test]
fn test_chain_not_supported() {
    let root_dir = tempdir().unwrap();
    let img1_dir = tempdir().unwrap();
    let img2_dir = tempdir().unwrap();
    let output = tempdir().unwrap();

    // Build the three layers.
    write_file(root_dir.path(), "file.txt", b"root content");
    write_file(img1_dir.path(), "file.txt", b"img-1 updated content");
    write_file(
        img2_dir.path(),
        "file.txt",
        b"img-2 further updated content",
    );

    let (storage, compressor) = make_compressor();

    // img-root: no base.
    save_meta_with_base(&*storage, "img-root", None);

    // Compress img-1 relative to img-root.
    compressor
        .compress(
            root_dir.path(),
            img1_dir.path(),
            compress_opts("img-1", Some("img-root")),
        )
        .unwrap();

    // img-1 meta is stored by compress(), but we need base_image_id in storage
    // so chain detection can verify the chain.  save_image_meta again to be
    // explicit (compress() saves it, so this is a no-op except clarity).
    save_meta_with_base(&*storage, "img-1", Some("img-root"));

    // Compress img-2 relative to img-1 (compressing a chain is allowed).
    compressor
        .compress(
            img1_dir.path(),
            img2_dir.path(),
            compress_opts("img-2", Some("img-1")),
        )
        .unwrap();

    // Attempt to decompress img-2 — img-1 is itself a delta, so this must fail.
    let err = compressor
        .decompress(output.path(), decompress_opts("img-2", img1_dir.path()))
        .expect_err("decompressing a chained image should fail");

    let err_str = err.to_string().to_lowercase();
    assert!(
        err_str.contains("chain"),
        "error message should mention 'chain', got: {err_str}"
    );
}
