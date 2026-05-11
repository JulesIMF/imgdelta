# Provider Integration

This chapter shows how to embed `image-delta-core` directly in your own Rust
codebase, bypassing the `imgdelta` CLI.

## Add the dependency

```toml
# Cargo.toml
[dependencies]
image-delta-core = { git = "https://github.com/JulesIMF/imgdelta" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
async-trait = "0.1"
uuid = { version = "1", features = ["v4", "v5"] }
anyhow = "1"
```

## Implement `Storage`

`Storage` is an async trait. All methods must be safe to call concurrently
from multiple threads.

```rust
use async_trait::async_trait;
use image_delta_core::{
    Storage, BlobCandidate, ImageMeta, ImageStatus, Result,
};
use uuid::Uuid;

pub struct MyStorage {
    // your S3 client, DB pool, or any other backend
}

#[async_trait]
impl Storage for MyStorage {
    // ── Blob CAS ──────────────────────────────────────────────────────────

    async fn blob_exists(&self, sha256: &str) -> Result<Option<Uuid>> {
        // look up sha256 in your index; return Some(uuid) if found
        todo!()
    }

    async fn upload_blob(&self, sha256: &str, data: &[u8]) -> Result<Uuid> {
        // must be idempotent: same sha256 → same uuid, no duplicate write
        let uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, sha256.as_bytes());
        // store data in your object store under uuid.to_string()
        Ok(uuid)
    }

    async fn download_blob(&self, blob_id: Uuid) -> Result<Vec<u8>> {
        todo!()
    }

    // ── Manifest / patches ────────────────────────────────────────────────

    async fn upload_manifest(&self, image_id: &str, bytes: &[u8]) -> Result<()> {
        todo!()
    }
    async fn download_manifest(&self, image_id: &str) -> Result<Vec<u8>> {
        todo!()
    }
    async fn upload_patches(&self, image_id: &str, data: &[u8], _compressed: bool) -> Result<()> {
        todo!()
    }
    async fn download_patches(&self, image_id: &str) -> Result<Vec<u8>> {
        todo!()
    }

    // ── Image metadata ────────────────────────────────────────────────────

    async fn register_image(&self, meta: &ImageMeta) -> Result<()> { todo!() }
    async fn get_image(&self, image_id: &str) -> Result<Option<ImageMeta>> { todo!() }
    async fn update_status(&self, image_id: &str, status: ImageStatus) -> Result<()> { todo!() }
    async fn list_images(&self) -> Result<Vec<ImageMeta>> { todo!() }

    // ── Blob origins (needed for cross-image delta reuse) ─────────────────

    async fn find_blob_candidates(
        &self,
        base_image_id: &str,
        partition_number: Option<i32>,
    ) -> Result<Vec<BlobCandidate>> {
        // Return BlobCandidate { uuid, sha256, original_path } for each
        // file stored during the base image's compression.
        // Used by blob_lookup stage to find delta bases for new files.
        todo!()
    }

    async fn record_blob_origin(
        &self,
        blob_uuid: Uuid,
        orig_image_id: &str,
        base_image_id: Option<&str>,
        partition_number: Option<i32>,
        file_path: &str,
    ) -> Result<()> {
        todo!()
    }

    // ── Delete ────────────────────────────────────────────────────────────

    async fn delete_manifest(&self, image_id: &str) -> Result<()> { todo!() }
    async fn delete_patches(&self, image_id: &str) -> Result<()> { todo!() }
    async fn delete_blob(&self, blob_id: Uuid) -> Result<()> { todo!() }
    async fn delete_blob_origins(&self, image_id: &str) -> Result<()> { todo!() }
    async fn delete_image_meta(&self, image_id: &str) -> Result<()> { todo!() }
}
```

## Build a `RouterEncoder`

```rust
use std::sync::Arc;
use image_delta_core::{
    encoding::{PassthroughEncoder, PatchEncoder},
    ElfRule, GlobRule, RouterEncoder, RoutingRule,
    encoding::Xdelta3Encoder,
};

fn make_router() -> Arc<RouterEncoder> {
    let passthrough = Arc::new(PassthroughEncoder::new());
    let xdelta3     = Arc::new(Xdelta3Encoder::new());

    let rules: Vec<Box<dyn RoutingRule>> = vec![
        // Already-compressed → passthrough
        Box::new(GlobRule::new(
            "**/*.{gz,zst,xz,bz2}",
            Arc::clone(&passthrough) as Arc<dyn PatchEncoder>,
        )),
        // ELF binaries → xdelta3
        Box::new(ElfRule::new(
            Arc::clone(&xdelta3) as Arc<dyn PatchEncoder>,
        )),
    ];

    // Fallback: xdelta3 for everything else
    Arc::new(RouterEncoder::new(rules, xdelta3))
}
```

## Compress an image

```rust
use std::sync::Arc;
use std::path::Path;
use image_delta_core::{
    DirectoryImage, Image,
    operations::compress,
    operations::CompressOptions,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let storage = Arc::new(MyStorage { /* … */ });
    let router  = make_router();
    let image_format: Arc<dyn Image> = Arc::new(DirectoryImage);

    let stats = compress(
        Arc::clone(&image_format),
        Arc::clone(&storage),
        Arc::clone(&router),
        Path::new("/mnt/base-image"),    // source root
        Path::new("/mnt/target-image"),  // target root
        CompressOptions {
            image_id:             "debian-11-20260502".into(),
            base_image_id:        Some("debian-11-20260401".into()),
            workers:              8,
            passthrough_threshold: 1.0,
            overwrite:            false,
            debug_dir:            None,
        },
    ).await?;

    println!(
        "patched={} added={} removed={} ratio={:.3}",
        stats.files_patched,
        stats.files_added,
        stats.files_removed,
        stats.ratio(),
    );
    Ok(())
}
```

## Decompress an image

```rust
use image_delta_core::{
    operations::decompress,
    operations::DecompressOptions,
};

let stats = decompress(
    Arc::clone(&image_format),
    Arc::clone(&storage),
    Arc::clone(&router),
    DecompressOptions {
        image_id:  "debian-11-20260502".into(),
        base_root: "/mnt/base-image".into(),
        workers:   8,
    },
    Path::new("/mnt/restored"),   // output directory
).await?;
```

## Verify correctness after decompression

The integration test helpers in `core/tests/common/` include a `compare_dirs`
function that checks:

- File content (SHA-256)
- Unix mode, uid, gid
- Modification time (±1 s tolerance)
- Symlink targets
- Hardlink relationships `(dev, ino)`
- Extended attributes (`xattr`)

Copy or adapt it for your own integration tests:

```rust
let diffs = compare_dirs(&target_root, &output_dir);
assert!(
    diffs.is_empty(),
    "decompression introduced differences: {diffs:#?}"
);
```

## Custom image format

If your images use a format other than plain directories or qcow2, implement
`Image` and `OpenImage`:

```rust
use image_delta_core::{Image, OpenImage, partitions::{DiskLayout, PartitionHandle}};

pub struct MyImageFormat;

impl Image for MyImageFormat {
    fn format_name(&self) -> &'static str { "my-format" }

    fn open(&self, path: &std::path::Path)
        -> image_delta_core::Result<Box<dyn OpenImage>>
    {
        // mount / parse the image; return an OpenImage that keeps
        // OS resources alive until dropped
        todo!()
    }
}

struct MyOpenImage {
    layout: DiskLayout,
    // NBD connection, loop device, FUSE handle, …
}

impl OpenImage for MyOpenImage {
    fn disk_layout(&self) -> &DiskLayout { &self.layout }

    fn partitions(&self)
        -> image_delta_core::Result<Vec<PartitionHandle>>
    {
        // return one PartitionHandle per partition
        todo!()
    }
}
```
