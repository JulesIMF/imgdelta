// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// S3Storage: AWS S3 / compatible object-store backed Storage implementation

// Phase 5: S3 + PostgreSQL implementation of the Storage trait.
#![allow(dead_code)] // S3Storage is wired in Part E (CLI commands); used in integration tests now.
//!
//! ## S3 key layout
//! ```text
//! blobs/{uuid}                        — raw blob bytes (CAS)
//! images/{image_id}/manifest.msgpack  — serialised Manifest
//! images/{image_id}/patches.tar       — patches tar archive
//! ```
//!
//! ## PostgreSQL tables
//! See `migrations/` for DDL:
//! - `images`       — image metadata + lifecycle status
//! - `blob_origins` — file provenance: which image each blob came from
//! - `blob_index`   — sha256 → uuid CAS index for deduplication

use anyhow::Context as _;
use async_trait::async_trait;
use aws_sdk_s3::primitives::ByteStream;
use sqlx::Row as _;
use uuid::Uuid;

use image_delta_core::storage::{BlobCandidate, ImageMeta, ImageStatus, Storage};
use image_delta_core::{Error, Result};

use crate::config::StorageConfig;

// ── Key builders ─────────────────────────────────────────────────────────────

fn blob_key(blob_id: Uuid) -> String {
    format!("blobs/{blob_id}")
}

fn manifest_key(image_id: &str) -> String {
    format!("images/{image_id}/manifest.msgpack")
}

fn patches_key(image_id: &str) -> String {
    format!("images/{image_id}/patches.tar")
}

// ── Struct ───────────────────────────────────────────────────────────────────

pub struct S3Storage {
    s3: aws_sdk_s3::Client,
    pg: sqlx::PgPool,
    bucket: String,
}

impl S3Storage {
    /// Connect to S3 + PostgreSQL, run pending migrations, ensure bucket exists.
    ///
    /// Credentials are loaded from environment variables:
    /// `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`.
    pub async fn new(cfg: &StorageConfig) -> anyhow::Result<Self> {
        let (s3_bucket, s3_region, s3_endpoint, database_url) = match cfg {
            StorageConfig::S3 {
                s3_bucket,
                s3_region,
                s3_endpoint,
                database_url,
            } => (s3_bucket, s3_region, s3_endpoint, database_url),
            _ => anyhow::bail!("S3Storage requires an S3 storage config"),
        };

        // ── AWS / S3 client ───────────────────────────────────────────────────
        let region = aws_sdk_s3::config::Region::new(
            s3_region.clone().unwrap_or_else(|| "us-east-1".into()),
        );
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(region)
            .load()
            .await;

        let mut s3_builder = aws_sdk_s3::config::Builder::from(&sdk_config);
        if let Some(endpoint) = s3_endpoint {
            s3_builder = s3_builder.endpoint_url(endpoint).force_path_style(true);
        }
        let s3 = aws_sdk_s3::Client::from_conf(s3_builder.build());

        // ── PostgreSQL pool ───────────────────────────────────────────────────
        let pg = sqlx::PgPool::connect(database_url)
            .await
            .with_context(|| format!("connect to postgres: {database_url}"))?;

        // Run migrations (SQL embedded at compile time via sqlx::migrate!)
        sqlx::migrate!("./migrations")
            .run(&pg)
            .await
            .context("run sqlx migrations")?;

        let storage = Self {
            s3,
            pg,
            bucket: s3_bucket.clone(),
        };

        storage
            .ensure_bucket()
            .await
            .with_context(|| format!("ensure bucket '{s3_bucket}'"))?;

        Ok(storage)
    }

    /// Create the S3 bucket if it does not already exist.
    async fn ensure_bucket(&self) -> anyhow::Result<()> {
        use aws_sdk_s3::error::SdkError;
        use aws_sdk_s3::operation::create_bucket::CreateBucketError;

        let exists = self
            .s3
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .is_ok();
        if !exists {
            match self.s3.create_bucket().bucket(&self.bucket).send().await {
                Ok(_) => {}
                Err(SdkError::ServiceError(e)) => match e.err() {
                    // Bucket was concurrently created (parallel tests / idempotent re-run)
                    CreateBucketError::BucketAlreadyOwnedByYou(_)
                    | CreateBucketError::BucketAlreadyExists(_) => {}
                    err => anyhow::bail!("create_bucket '{}': {err}", self.bucket),
                },
                Err(e) => anyhow::bail!("create_bucket '{}': {e}", self.bucket),
            }
        }
        Ok(())
    }

    /// Download raw bytes for an S3 key.
    async fn get_object_bytes(&self, key: &str) -> Result<Vec<u8>> {
        let resp = self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| Error::Storage(format!("get_object({key}): {e}")))?;

        let bytes = resp
            .body
            .collect()
            .await
            .map_err(|e| Error::Storage(format!("read_body({key}): {e}")))?
            .into_bytes();

        Ok(bytes.to_vec())
    }

    /// Upload raw bytes to an S3 key.
    async fn put_object_bytes(&self, key: &str, data: Vec<u8>) -> Result<()> {
        self.s3
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("put_object({key}): {e}")))?;
        Ok(())
    }
}

// ── Storage trait implementation ─────────────────────────────────────────────

#[async_trait]
impl Storage for S3Storage {
    async fn blob_exists(&self, sha256: &str) -> Result<Option<Uuid>> {
        let row = sqlx::query("SELECT blob_id FROM blob_index WHERE sha256 = $1")
            .bind(sha256)
            .fetch_optional(&self.pg)
            .await
            .map_err(|e| Error::Storage(format!("blob_exists: {e}")))?;

        Ok(row.map(|r| r.get::<Uuid, _>("blob_id")))
    }

    /// Upload blob bytes, deduplicating by SHA-256.
    ///
    /// Uses UUID v5 (OID namespace + sha256) for a deterministic blob ID so
    /// concurrent uploads of the same content are fully idempotent.
    async fn upload_blob(&self, sha256: &str, data: &[u8]) -> Result<Uuid> {
        let blob_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, sha256.as_bytes());

        self.put_object_bytes(&blob_key(blob_id), data.to_vec())
            .await?;

        sqlx::query(
            "INSERT INTO blob_index (sha256, blob_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(sha256)
        .bind(blob_id)
        .execute(&self.pg)
        .await
        .map_err(|e| Error::Storage(format!("upload_blob index: {e}")))?;

        Ok(blob_id)
    }

    async fn download_blob(&self, blob_id: Uuid) -> Result<Vec<u8>> {
        self.get_object_bytes(&blob_key(blob_id)).await
    }

    async fn upload_manifest(&self, image_id: &str, manifest_bytes: &[u8]) -> Result<()> {
        self.put_object_bytes(&manifest_key(image_id), manifest_bytes.to_vec())
            .await
    }

    async fn download_manifest(&self, image_id: &str) -> Result<Vec<u8>> {
        self.get_object_bytes(&manifest_key(image_id)).await
    }

    async fn upload_patches(&self, image_id: &str, data: &[u8], _compressed: bool) -> Result<()> {
        self.put_object_bytes(&patches_key(image_id), data.to_vec())
            .await
    }

    async fn download_patches(&self, image_id: &str) -> Result<Vec<u8>> {
        self.get_object_bytes(&patches_key(image_id)).await
    }

    async fn register_image(&self, meta: &ImageMeta) -> Result<()> {
        sqlx::query("INSERT INTO images (image_id, base_image_id, format) VALUES ($1, $2, $3)")
            .bind(&meta.image_id)
            .bind(&meta.base_image_id)
            .bind(&meta.format)
            .execute(&self.pg)
            .await
            .map_err(|e| Error::Storage(format!("register_image: {e}")))?;
        Ok(())
    }

    async fn get_image(&self, image_id: &str) -> Result<Option<ImageMeta>> {
        let row = sqlx::query(
            "SELECT image_id, base_image_id, format, status FROM images WHERE image_id = $1",
        )
        .bind(image_id)
        .fetch_optional(&self.pg)
        .await
        .map_err(|e| Error::Storage(format!("get_image: {e}")))?;

        Ok(row.map(|r| ImageMeta {
            image_id: r.get("image_id"),
            base_image_id: r.get("base_image_id"),
            format: r.get("format"),
            status: r.get("status"),
        }))
    }

    async fn update_status(&self, image_id: &str, status: ImageStatus) -> Result<()> {
        let (status_str, detail): (&str, Option<&str>) = match &status {
            ImageStatus::Pending => ("pending", None),
            ImageStatus::Compressing => ("compressing", None),
            ImageStatus::Compressed => ("compressed", None),
            ImageStatus::Failed(msg) => ("failed", Some(msg.as_str())),
        };

        sqlx::query("UPDATE images SET status = $1, status_detail = $2 WHERE image_id = $3")
            .bind(status_str)
            .bind(detail)
            .bind(image_id)
            .execute(&self.pg)
            .await
            .map_err(|e| Error::Storage(format!("update_status: {e}")))?;

        Ok(())
    }

    async fn list_images(&self) -> Result<Vec<ImageMeta>> {
        let rows = sqlx::query(
            "SELECT image_id, base_image_id, format, status FROM images ORDER BY created_at",
        )
        .fetch_all(&self.pg)
        .await
        .map_err(|e| Error::Storage(format!("list_images: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| ImageMeta {
                image_id: r.get("image_id"),
                base_image_id: r.get("base_image_id"),
                format: r.get("format"),
                status: r.get("status"),
            })
            .collect())
    }

    async fn find_blob_candidates(&self, base_image_id: &str) -> Result<Vec<BlobCandidate>> {
        let rows = sqlx::query(
            "SELECT bo.blob_id, bi.sha256, bo.path \
             FROM blob_origins bo \
             JOIN blob_index bi ON bi.blob_id = bo.blob_id \
             WHERE bo.orig_image_id = $1 \
             ORDER BY bo.created_at DESC",
        )
        .bind(base_image_id)
        .fetch_all(&self.pg)
        .await
        .map_err(|e| Error::Storage(format!("find_blob_candidates: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| BlobCandidate {
                uuid: r.get::<Uuid, _>("blob_id"),
                sha256: r.get("sha256"),
                original_path: r.get("path"),
            })
            .collect())
    }

    async fn record_blob_origin(
        &self,
        blob_uuid: Uuid,
        orig_image_id: &str,
        base_image_id: Option<&str>,
        file_path: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO blob_origins (blob_id, orig_image_id, base_image_id, path, size) \
             VALUES ($1, $2, $3, $4, 0) ON CONFLICT (blob_id, orig_image_id) DO NOTHING",
        )
        .bind(blob_uuid)
        .bind(orig_image_id)
        .bind(base_image_id)
        .bind(file_path)
        .execute(&self.pg)
        .await
        .map_err(|e| Error::Storage(format!("record_blob_origin: {e}")))?;
        Ok(())
    }
}

// ── Integration tests ─────────────────────────────────────────────────────────
//
// Requires running docker-compose services:
//   docker compose up -d postgres minio
//
// Run with:
//   cargo test -p image-delta-cli -- --ignored
//
// Environment defaults (match docker-compose):
//   TEST_DATABASE_URL     = postgres://imgdelta:imgdelta@localhost/imgdelta
//   TEST_S3_ENDPOINT      = http://localhost:9000
//   TEST_S3_BUCKET        = imgdelta-test
//   AWS_ACCESS_KEY_ID     = minioadmin
//   AWS_SECRET_ACCESS_KEY = minioadmin

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StorageConfig;

    fn test_cfg() -> StorageConfig {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://imgdelta:imgdelta@localhost/imgdelta".into());
        let s3_endpoint =
            std::env::var("TEST_S3_ENDPOINT").unwrap_or_else(|_| "http://localhost:9000".into());
        let s3_bucket = std::env::var("TEST_S3_BUCKET").unwrap_or_else(|_| "imgdelta-test".into());

        // Provide default MinIO credentials if not already set in environment
        if std::env::var("AWS_ACCESS_KEY_ID").is_err() {
            // Safety: test-only; sets process env vars before the AWS SDK reads them
            unsafe {
                std::env::set_var("AWS_ACCESS_KEY_ID", "minioadmin");
                std::env::set_var("AWS_SECRET_ACCESS_KEY", "minioadmin");
            }
        }

        StorageConfig::S3 {
            s3_bucket,
            s3_region: Some("us-east-1".into()),
            s3_endpoint: Some(s3_endpoint),
            database_url,
        }
    }

    async fn make_storage() -> S3Storage {
        S3Storage::new(&test_cfg())
            .await
            .expect("S3Storage::new failed")
    }

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::Digest as _;
        hex::encode(sha2::Sha256::digest(data))
    }

    // ── Blob CAS ─────────────────────────────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires docker-compose postgres + minio"]
    async fn test_blob_upload_download_roundtrip() {
        let storage = make_storage().await;
        // Use a UUID-tagged payload so each test run has a fresh blob in the CAS.
        let tag = Uuid::new_v4();
        let data = format!("hello imgdelta blob content {tag}").into_bytes();
        let sha256 = sha256_hex(&data);

        assert!(storage.blob_exists(&sha256).await.unwrap().is_none());

        let uuid1 = storage.upload_blob(&sha256, &data).await.unwrap();
        assert_eq!(storage.blob_exists(&sha256).await.unwrap(), Some(uuid1));

        // Idempotent
        let uuid2 = storage.upload_blob(&sha256, &data).await.unwrap();
        assert_eq!(uuid1, uuid2);

        let downloaded = storage.download_blob(uuid1).await.unwrap();
        assert_eq!(downloaded, data);
    }

    // ── Manifest / patches ────────────────────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires docker-compose postgres + minio"]
    async fn test_manifest_roundtrip() {
        let storage = make_storage().await;
        let image_id = format!("img-manifest-{}", &Uuid::new_v4().to_string()[..8]);
        let payload = b"fake manifest bytes";

        storage.upload_manifest(&image_id, payload).await.unwrap();
        assert_eq!(
            storage.download_manifest(&image_id).await.unwrap(),
            payload.to_vec()
        );
    }

    #[tokio::test]
    #[ignore = "requires docker-compose postgres + minio"]
    async fn test_patches_roundtrip() {
        let storage = make_storage().await;
        let image_id = format!("img-patches-{}", &Uuid::new_v4().to_string()[..8]);
        let payload = b"fake patches tar bytes";

        storage
            .upload_patches(&image_id, payload, false)
            .await
            .unwrap();
        assert_eq!(
            storage.download_patches(&image_id).await.unwrap(),
            payload.to_vec()
        );
    }

    // ── Image metadata ────────────────────────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires docker-compose postgres + minio"]
    async fn test_register_get_list_images() {
        let storage = make_storage().await;
        let image_id = format!("img-reg-{}", &Uuid::new_v4().to_string()[..8]);

        assert!(storage.get_image(&image_id).await.unwrap().is_none());

        let meta = ImageMeta {
            image_id: image_id.clone(),
            base_image_id: None,
            format: "directory".into(),
            status: "pending".into(),
        };
        storage.register_image(&meta).await.unwrap();

        let found = storage.get_image(&image_id).await.unwrap().unwrap();
        assert_eq!(found.image_id, image_id);
        assert_eq!(found.format, "directory");
        assert!(found.base_image_id.is_none());

        assert!(storage
            .list_images()
            .await
            .unwrap()
            .iter()
            .any(|m| m.image_id == image_id));
    }

    #[tokio::test]
    #[ignore = "requires docker-compose postgres + minio"]
    async fn test_update_status_lifecycle() {
        let storage = make_storage().await;
        let image_id = format!("img-status-{}", &Uuid::new_v4().to_string()[..8]);

        storage
            .register_image(&ImageMeta {
                image_id: image_id.clone(),
                base_image_id: None,
                format: "qcow2".into(),
                status: "pending".into(),
            })
            .await
            .unwrap();

        for status in [
            ImageStatus::Compressing,
            ImageStatus::Compressed,
            ImageStatus::Failed("disk full".into()),
            ImageStatus::Pending,
        ] {
            storage.update_status(&image_id, status).await.unwrap();
        }
    }

    // ── Blob origins ──────────────────────────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires docker-compose postgres + minio"]
    async fn test_blob_origins_and_candidates() {
        let storage = make_storage().await;
        let image_id = format!("img-orig-{}", &Uuid::new_v4().to_string()[..8]);

        storage
            .register_image(&ImageMeta {
                image_id: image_id.clone(),
                base_image_id: None,
                format: "directory".into(),
                status: "pending".into(),
            })
            .await
            .unwrap();

        let data = b"binary content for origin test";
        let sha256 = sha256_hex(data);
        let blob_uuid = storage.upload_blob(&sha256, data).await.unwrap();

        storage
            .record_blob_origin(blob_uuid, &image_id, None, "usr/bin/tool")
            .await
            .unwrap();

        let candidates = storage.find_blob_candidates(&image_id).await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].uuid, blob_uuid);
        assert_eq!(candidates[0].sha256, sha256);
        assert_eq!(candidates[0].original_path, "usr/bin/tool");
    }
}
