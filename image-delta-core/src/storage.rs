use uuid::Uuid;

/// A blob candidate returned by storage when searching for a suitable delta base.
#[derive(Debug, Clone)]
pub struct BlobCandidate {
    pub blob_id: Uuid,
    /// Path this blob originated from (for path-match scoring).
    pub path: String,
    pub size: u64,
}

/// Lightweight metadata about an image known to storage.
#[derive(Debug, Clone)]
pub struct ImageMeta {
    pub image_id: String,
    pub base_image_id: Option<String>,
    pub format: String,
}

/// Lifecycle state of an image in storage.
#[derive(Debug, Clone)]
pub enum ImageStatus {
    Pending,
    Compressing,
    Compressed,
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

    fn save_image_meta(&self, meta: &ImageMeta) -> crate::Result<()>;

    fn get_image_meta(&self, image_id: &str) -> crate::Result<Option<ImageMeta>>;

    fn set_image_status(&self, image_id: &str, status: ImageStatus) -> crate::Result<()>;

    fn list_images(&self) -> crate::Result<Vec<ImageMeta>>;
}
