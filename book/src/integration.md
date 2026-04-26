# Provider Integration

This chapter shows how to embed `image-delta-core` as a library in your
own Rust codebase, bypassing the CLI and `S3Storage`.

## Add the dependency

```toml
# Cargo.toml
[dependencies]
image-delta-core = { git = "https://github.com/JulesIMF/imgdelta" }
```

## Implement `Storage`

```rust
use image_delta_core::{Storage, BlobCandidate, ImageMeta, ImageStatus, Result};
use uuid::Uuid;

pub struct MyStorage {
    // your S3 client, DB pool, …
}

impl Storage for MyStorage {
    fn upload_blob(&self, data: &[u8]) -> Result<Uuid> {
        let id = Uuid::new_v4();
        // store data in your object store under id.to_string()
        Ok(id)
    }

    fn download_blob(&self, blob_id: Uuid) -> Result<Vec<u8>> {
        // fetch from your object store
        todo!()
    }

    fn upload_manifest(&self, image_id: &str, bytes: &[u8]) -> Result<()> {
        todo!()
    }

    fn download_manifest(&self, image_id: &str) -> Result<Vec<u8>> {
        todo!()
    }

    fn find_blob_candidates(&self, base_image_id: &str) -> Result<Vec<BlobCandidate>> {
        // return BlobCandidate { blob_id, path, size } for each file
        // stored during the base image's compression
        todo!()
    }

    fn save_image_meta(&self, meta: &ImageMeta) -> Result<()> { todo!() }
    fn get_image_meta(&self, image_id: &str) -> Result<Option<ImageMeta>> { todo!() }
    fn set_image_status(&self, image_id: &str, status: ImageStatus) -> Result<()> { todo!() }
    fn list_images(&self) -> Result<Vec<ImageMeta>> { todo!() }
    fn upload_patches(&self, image_id: &str, data: &[u8]) -> Result<()> { todo!() }
    fn download_patches(&self, image_id: &str) -> Result<Vec<u8>> { todo!() }
}
```

## Wire up a compressor

```rust
use std::sync::Arc;
use image_delta_core::{
    DefaultCompressor, Xdelta3Encoder, PassthroughEncoder,
    RouterEncoder, GlobRule, ElfRule,
    CompressOptions, DecompressOptions,
};
use std::path::Path;

fn make_compressor(storage: Arc<dyn image_delta_core::Storage>)
    -> Arc<DefaultCompressor>
{
    // Build routing: already-compressed → passthrough, ELF → xdelta3, default → xdelta3
    let passthrough = Arc::new(PassthroughEncoder::new());
    let xdelta3     = Arc::new(Xdelta3Encoder::new());

    let rules: Vec<Box<dyn image_delta_core::RoutingRule>> = vec![
        Box::new(GlobRule::new("**/*.{gz,zst,xz}", Arc::clone(&passthrough))),
        Box::new(ElfRule::new(Arc::clone(&xdelta3))),
    ];
    let router = Arc::new(RouterEncoder::new(rules, Arc::clone(&xdelta3)));

    Arc::new(DefaultCompressor::new(storage, router))
}

fn compress_image(
    storage:       Arc<dyn image_delta_core::Storage>,
    base_root:     &Path,
    target_root:   &Path,
    image_id:      &str,
    base_image_id: Option<&str>,
) -> image_delta_core::Result<image_delta_core::CompressionStats> {
    let compressor = make_compressor(Arc::clone(&storage));

    let opts = CompressOptions {
        image_id:              image_id.to_string(),
        base_image_id:         base_image_id.map(str::to_string),
        workers:               8,
        passthrough_threshold: 1.0,
    };

    use image_delta_core::Compressor; // bring trait into scope
    compressor.compress(base_root, target_root, opts)
}

fn decompress_image(
    storage:    Arc<dyn image_delta_core::Storage>,
    image_id:   &str,
    base_root:  &Path,
    output_dir: &Path,
) -> image_delta_core::Result<image_delta_core::DecompressionStats> {
    let compressor = make_compressor(Arc::clone(&storage));

    let opts = DecompressOptions {
        image_id:  image_id.to_string(),
        base_root: base_root.to_path_buf(),
        workers:   8,
    };

    use image_delta_core::Compressor;
    compressor.decompress(output_dir, opts)
}
```

## Verify correctness

After decompression, use `compare_dirs` from the test helpers to assert
that the output matches the original target:

```rust
// in your integration test — copy compare_dirs from tests/common/mod.rs
// or use the assertion helpers from fs_diff_integration.rs as a template

let diffs = compare_dirs(&target_root, &output_dir);
assert!(diffs.is_empty(), "decompression introduced differences: {diffs:#?}");
```

The comparison checks: file content (SHA-256), Unix mode, uid, gid, mtime
(±1 s tolerance), file type, symlink targets, hardlink relationships, and
extended attributes (`xattr`).
