use uuid::Uuid;

/// A blob candidate returned by storage when searching for a suitable delta base.
#[derive(Debug, Clone)]
pub struct BlobCandidate {
    /// UUID assigned when the blob was uploaded.
    pub blob_id: Uuid,
    /// Path this blob originated from (for path-match scoring).
    pub path: String,
    /// Uncompressed byte size of this blob.
    pub size: u64,
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
/// - `upload_blob` must be idempotent: uploading the same bytes twice is fine.
pub trait Storage: Send + Sync {
    /// Upload raw bytes and return the assigned UUID.
    fn upload_blob(&self, data: &[u8]) -> crate::Result<Uuid>;

    /// Download raw bytes for a known blob UUID.
    fn download_blob(&self, blob_id: Uuid) -> crate::Result<Vec<u8>>;

    /// Upload a serialised manifest for `image_id`.
    fn upload_manifest(&self, image_id: &str, manifest_bytes: &[u8]) -> crate::Result<()>;

    /// Download the serialised manifest for `image_id`.
    fn download_manifest(&self, image_id: &str) -> crate::Result<Vec<u8>>;

    /// Return blobs from `base_image_id` that are candidates for delta encoding
    /// against files in the new image.
    fn find_blob_candidates(&self, base_image_id: &str) -> crate::Result<Vec<BlobCandidate>>;

    /// Persist metadata for a newly created or updated image.
    fn save_image_meta(&self, meta: &ImageMeta) -> crate::Result<()>;

    /// Look up metadata for an image by ID, returning `None` if not found.
    fn get_image_meta(&self, image_id: &str) -> crate::Result<Option<ImageMeta>>;

    /// Update the lifecycle status of an image.
    fn set_image_status(&self, image_id: &str, status: ImageStatus) -> crate::Result<()>;

    /// Return metadata for all known images.
    fn list_images(&self) -> crate::Result<Vec<ImageMeta>>;

    /// Upload the patches tar (or tar.gz) archive for `image_id`.
    fn upload_patches(&self, image_id: &str, data: &[u8]) -> crate::Result<()>;

    /// Download the patches tar (or tar.gz) archive for `image_id`.
    fn download_patches(&self, image_id: &str) -> crate::Result<Vec<u8>>;
}
