use async_trait::async_trait;
use uuid::Uuid;

/// A blob candidate returned by storage when searching for a suitable delta base.
///
/// Returned by [`Storage::find_blob_candidates`] and consumed by the path-matcher
/// to decide which base blob to use for a delta.
#[derive(Debug, Clone)]
pub struct BlobCandidate {
    /// UUID assigned when the blob was uploaded; used to call
    /// [`Storage::download_blob`].
    pub uuid: Uuid,
    /// SHA-256 hex digest of the blob bytes.  Used to verify integrity after
    /// download.
    pub sha256: String,
    /// Relative path this blob was recorded under via
    /// [`Storage::record_blob_origin`].  Used by the path-matcher for scoring.
    pub original_path: String,
}

/// Lightweight metadata about an image known to storage.
#[derive(Debug, Clone)]
pub struct ImageMeta {
    /// Provider-assigned identifier for this image.
    pub image_id: String,
    /// Identifier of the base image used as delta source, or `None` for root images.
    pub base_image_id: Option<String>,
    /// Image container format (`"directory"`, `"qcow2"`, …).
    pub format: String,
    /// Current lifecycle status string, e.g. `"pending"`, `"compressing"`,
    /// `"compressed"`, `"failed"`.
    pub status: String,
}

/// Lifecycle state of an image in storage.
#[derive(Debug, Clone)]
pub enum ImageStatus {
    /// Compression job created but not yet started.
    Pending,
    /// Compression is currently in progress.
    Compressing,
    /// Compression finished successfully; patches are available.
    Compressed,
    /// Compression failed; the inner string contains a human-readable reason.
    Failed(String),
}

/// Persistent storage backend abstraction.
///
/// S3 + PostgreSQL is the production implementation (in `image-delta-cli`).
/// [`FakeStorage`] (in-memory) is used for L1 unit/integration tests.
///
/// # Contract for implementors
///
/// - All methods must be safe to call concurrently from multiple threads.
/// - `upload_blob` must be idempotent: uploading the same bytes twice (same SHA-256)
///   must return the same UUID and not store a duplicate.
///
/// # Note
///
/// `save_stats` from the original design is intentionally omitted here to avoid a
/// circular dependency with `compressor::CompressionStats`.  It will be added in
/// Phase 5 via a separate `StoredStats` type.
#[async_trait]
pub trait Storage: Send + Sync {
    // ── Blob CAS ─────────────────────────────────────────────────────────────

    /// Check whether a blob with this SHA-256 digest already exists.
    ///
    /// Returns `Some(uuid)` if it exists, `None` otherwise.
    async fn blob_exists(&self, sha256: &str) -> crate::Result<Option<Uuid>>;

    /// Upload raw bytes, keyed by their SHA-256 hex digest.
    ///
    /// Must be idempotent: if a blob with `sha256` already exists, returns the
    /// existing UUID without re-uploading.
    async fn upload_blob(&self, sha256: &str, data: &[u8]) -> crate::Result<Uuid>;

    /// Download raw bytes for a known blob UUID.
    async fn download_blob(&self, blob_id: Uuid) -> crate::Result<Vec<u8>>;

    // ── Image data ────────────────────────────────────────────────────────────

    /// Upload a serialised manifest for `image_id`.
    async fn upload_manifest(&self, image_id: &str, manifest_bytes: &[u8]) -> crate::Result<()>;

    /// Download the serialised manifest for `image_id`.
    async fn download_manifest(&self, image_id: &str) -> crate::Result<Vec<u8>>;

    /// Upload the patches tar (or tar.gz) archive for `image_id`.
    ///
    /// `compressed` — `true` if the bytes are gzip-compressed (tar.gz).
    async fn upload_patches(
        &self,
        image_id: &str,
        data: &[u8],
        compressed: bool,
    ) -> crate::Result<()>;

    /// Download the patches tar (or tar.gz) archive for `image_id`.
    async fn download_patches(&self, image_id: &str) -> crate::Result<Vec<u8>>;

    // ── DB ────────────────────────────────────────────────────────────────────

    /// Persist metadata for a newly created or updated image.
    async fn register_image(&self, meta: &ImageMeta) -> crate::Result<()>;

    /// Look up metadata for an image by ID, returning `None` if not found.
    async fn get_image(&self, image_id: &str) -> crate::Result<Option<ImageMeta>>;

    /// Update the lifecycle status of an image.
    async fn update_status(&self, image_id: &str, status: ImageStatus) -> crate::Result<()>;

    /// Return metadata for all known images.
    async fn list_images(&self) -> crate::Result<Vec<ImageMeta>>;

    // ── BlobPatch ─────────────────────────────────────────────────────────────

    /// Return blobs from `base_image_id` that are candidates for delta encoding
    /// against files in the new image.
    async fn find_blob_candidates(&self, base_image_id: &str) -> crate::Result<Vec<BlobCandidate>>;

    /// Record that `blob_uuid` originated from `file_path` in `image_id`.
    ///
    /// Called by [`DefaultCompressor`] after each `upload_blob` so that future
    /// compress operations can use this blob as a delta base via
    /// [`find_blob_candidates`].
    ///
    /// [`DefaultCompressor`]: crate::DefaultCompressor
    /// [`find_blob_candidates`]: Storage::find_blob_candidates
    async fn record_blob_origin(
        &self,
        blob_uuid: Uuid,
        image_id: &str,
        file_path: &str,
    ) -> crate::Result<()>;
}
