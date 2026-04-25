use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Top-level manifest describing how a target image was compressed relative to
/// a base image.  Serialised as MessagePack for storage; JSON for debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub header: ManifestHeader,
    pub entries: Vec<Entry>,
}

/// Immutable metadata written once at compression time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestHeader {
    /// Provider-assigned identifier for the compressed image.
    pub image_id: String,
    /// Provider-assigned identifier for the base image used as delta source.
    pub base_image_id: Option<String>,
    /// Image format used at compression time (`"directory"`, `"qcow2"`, …).
    pub format: String,
    /// Unix timestamp (seconds) when the manifest was created.
    pub created_at: u64,
}

/// One entry in the manifest — corresponds to a single path in the filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the filesystem root, UTF-8, forward-slash separated.
    pub path: String,
    pub entry_type: EntryType,
    /// Uncompressed size in bytes (0 for directories/symlinks).
    pub size: u64,
    /// SHA-256 of the file content, if applicable.
    pub sha256: Option<[u8; 32]>,
    /// Unix permission bits.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Modification time as Unix timestamp (seconds).
    pub mtime: i64,
    /// Present when this file was stored as a delta against a base blob.
    pub delta: Option<DeltaRef>,
    /// Present when this file was stored as a verbatim blob (no base, or passthrough).
    pub blob: Option<BlobRef>,
    /// Symlink target, present when `entry_type == EntryType::Symlink`.
    pub link_target: Option<String>,
    /// Path of the first occurrence, present when `entry_type == EntryType::Hardlink`.
    pub hardlink_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    File,
    Directory,
    Symlink,
    Hardlink,
    Other,
}

/// Reference to a delta blob and its base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaRef {
    /// UUID of the blob containing the delta bytes.
    pub blob_id: Uuid,
    /// UUID of the base blob that must be present to decode.
    pub base_blob_id: Uuid,
    /// Algorithm identifier matching [`DeltaEncoder::algorithm_id`].
    pub algorithm: String,
    /// Size of the stored (compressed) delta in bytes.
    pub compressed_size: u64,
}

/// Reference to a verbatim (non-delta) blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobRef {
    pub blob_id: Uuid,
    pub size: u64,
}
