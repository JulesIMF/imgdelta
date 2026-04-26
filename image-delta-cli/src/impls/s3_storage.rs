// Phase 5: S3 + PostgreSQL implementation of the Storage trait.
// Fully implemented in Phase 5; stubs are dead code until then.
#![allow(dead_code)]
//
// Dependencies (added to Cargo.toml in Phase 5):
//   aws-sdk-s3, aws-config  — S3 operations
//   sqlx (postgres feature) — PostgreSQL metadata index
//   tokio                   — async runtime (only here, not in core)
//
// S3 layout:
//   blobs/{uuid}                        — raw blob bytes
//   images/{image_id}/manifest.msgpack  — serialised Manifest
//
// PostgreSQL tables:  see docs/mpv/arch/stage3-decisions.md

use async_trait::async_trait;
use image_delta_core::{BlobCandidate, ImageMeta, ImageStatus, Result, Storage};
use uuid::Uuid;

pub struct S3Storage {
    // Phase 5: s3_client, pg_pool, bucket
    _private: (),
}

impl S3Storage {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

#[async_trait]
impl Storage for S3Storage {
    async fn blob_exists(&self, _sha256: &str) -> Result<Option<Uuid>> {
        todo!("Phase 5: sqlx SELECT uuid FROM blob_sha256_index WHERE sha256 = $1")
    }

    async fn upload_blob(&self, _sha256: &str, _data: &[u8]) -> Result<Uuid> {
        todo!("Phase 5: aws-sdk-s3 put_object + sqlx INSERT sha256_index")
    }

    async fn download_blob(&self, _blob_id: Uuid) -> Result<Vec<u8>> {
        todo!("Phase 5: aws-sdk-s3 get_object")
    }

    async fn upload_manifest(&self, _image_id: &str, _manifest_bytes: &[u8]) -> Result<()> {
        todo!("Phase 5: aws-sdk-s3 put_object")
    }

    async fn download_manifest(&self, _image_id: &str) -> Result<Vec<u8>> {
        todo!("Phase 5: aws-sdk-s3 get_object")
    }

    async fn upload_patches(&self, _image_id: &str, _data: &[u8], _compressed: bool) -> Result<()> {
        todo!("Phase 5: aws-sdk-s3 put_object patches.tar")
    }

    async fn download_patches(&self, _image_id: &str) -> Result<Vec<u8>> {
        todo!("Phase 5: aws-sdk-s3 get_object patches.tar")
    }

    async fn register_image(&self, _meta: &ImageMeta) -> Result<()> {
        todo!("Phase 5: sqlx INSERT into images")
    }

    async fn get_image(&self, _image_id: &str) -> Result<Option<ImageMeta>> {
        todo!("Phase 5: sqlx SELECT from images")
    }

    async fn update_status(&self, _image_id: &str, _status: ImageStatus) -> Result<()> {
        todo!("Phase 5: sqlx UPDATE images SET status")
    }

    async fn list_images(&self) -> Result<Vec<ImageMeta>> {
        todo!("Phase 5: sqlx SELECT from images")
    }

    async fn find_blob_candidates(&self, _base_image_id: &str) -> Result<Vec<BlobCandidate>> {
        todo!("Phase 5: sqlx SELECT from blob_origins")
    }

    async fn record_blob_origin(
        &self,
        _blob_uuid: Uuid,
        _image_id: &str,
        _file_path: &str,
    ) -> Result<()> {
        todo!("Phase 5: sqlx INSERT into blob_origins")
    }
}
