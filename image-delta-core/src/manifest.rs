use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AlgorithmCode;

/// Manifest format version stored in every [`ManifestHeader`].
///
/// Increment this constant on every breaking schema change so that older
/// decompressors can refuse to process manifests they don't understand.
pub const MANIFEST_VERSION: u32 = 2;

/// Returns `true` if `b` is `false` — used as `skip_serializing_if` predicate
/// for boolean fields that should be omitted when their value is the zero/default.
fn is_false(b: &bool) -> bool {
    !b
}

/// Top-level manifest describing how a target image was compressed relative to
/// a base image.  Serialised as MessagePack for storage; JSON for debugging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub header: ManifestHeader,
    pub entries: Vec<Entry>,
}

impl Manifest {
    /// Deserialize a manifest from the MessagePack bytes returned by
    /// [`Storage::download_manifest`].
    pub fn from_bytes(bytes: &[u8]) -> crate::Result<Self> {
        rmp_serde::from_slice(bytes).map_err(|e| crate::Error::Manifest(e.to_string()))
    }
}

/// Immutable metadata written once at compression time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestHeader {
    /// Manifest schema version — always [`MANIFEST_VERSION`] when writing.
    /// Readers must reject manifests with an unsupported version.
    pub version: u32,
    /// Provider-assigned identifier for the compressed image.
    pub image_id: String,
    /// Provider-assigned identifier for the base image used as delta source.
    /// `None` for root images that have no base.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_image_id: Option<String>,
    /// Image format used at compression time (`"directory"`, `"qcow2"`, …).
    pub format: String,
    /// Unix timestamp (seconds) when the manifest was created.
    pub created_at: u64,
    /// `true` → patches stored as `patches.tar.gz`;
    /// `false` → patches stored as `patches.tar`.
    ///
    /// Stored in the manifest so that decompression works without querying
    /// the database.
    pub patches_compressed: bool,
}

/// One entry in the manifest — corresponds to a single changed path in the
/// filesystem.  Unchanged files are **not** recorded; they are taken from the
/// base image as-is during decompression.
///
/// Interpretation table:
///
/// | `blob` | `patch` | Meaning                                                 |
/// |--------|---------|---------------------------------------------------------|
/// | None   | None    | Only metadata changed (chmod, chown, rename, …)         |
/// | Some   | None    | New file — content = blob in its entirety               |
/// | None   | Some    | Existing file patched against its counterpart in base   |
/// | Some   | Some    | BlobPatch: download blob, apply patch on top            |
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Path relative to the filesystem root, UTF-8, forward-slash separated.
    pub path: String,
    pub entry_type: EntryType,
    /// Uncompressed size in bytes (0 for directories and symlinks).
    pub size: u64,
    /// Present when this file's content was stored as a verbatim blob.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub blob: Option<BlobRef>,
    /// Present when a VCDIFF patch was computed for this file.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub patch: Option<PatchRef>,
    /// Changed filesystem attributes.  `None` when only content changed and
    /// all metadata (mode, uid, gid, mtime) is identical to the base image.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metadata: Option<Metadata>,
    /// Path of the link target, present when `entry_type == EntryType::Hardlink`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hardlink_target: Option<String>,
    /// `true` when this path was deleted relative to the base image.
    #[serde(default, skip_serializing_if = "is_false")]
    pub removed: bool,
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

/// Reference to an entry inside the `patches.tar[.gz]` archive that is stored
/// alongside the manifest in S3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PatchRef {
    /// Name of the member file inside the patches tar archive.
    pub archive_entry: String,
    /// Lowercase hex SHA-256 of the patch bytes — verified after extraction.
    pub sha256: String,
    /// Compact one-byte algorithm code — primary decoder lookup key.
    ///
    /// When `algorithm_code == AlgorithmCode::Extended`, the string
    /// `algorithm_id` field identifies the algorithm instead.
    pub algorithm_code: AlgorithmCode,
    /// Human-readable algorithm identifier.
    ///
    /// `None` for all built-in algorithms (those with a known
    /// [`AlgorithmCode`]).  `Some` only when
    /// `algorithm_code == AlgorithmCode::Extended`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub algorithm_id: Option<String>,
}

/// Reference to a verbatim (non-delta) blob stored in the blob store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlobRef {
    pub blob_id: Uuid,
    pub size: u64,
}

/// Filesystem attributes that changed relative to the base image.
///
/// Only the fields that actually changed are populated; `None` means "same as
/// base image, nothing to do during decompression".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Metadata {
    /// New path after a rename.  Applied **after** content is written.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub new_path: Option<String>,
    /// Unix permission bits.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gid: Option<u32>,
    /// Modification time as Unix timestamp (seconds).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mtime: Option<i64>,
    /// Symlink target string, present when `entry_type == EntryType::Symlink`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link_target: Option<String>,
    /// Extended attributes (e.g. security capabilities).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub xattrs: Option<HashMap<String, Vec<u8>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_header() -> ManifestHeader {
        ManifestHeader {
            version: MANIFEST_VERSION,
            image_id: "img-test-001".into(),
            base_image_id: Some("img-base-001".into()),
            format: "directory".into(),
            created_at: 1_714_000_000,
            patches_compressed: false,
        }
    }

    fn make_manifest(entries: Vec<Entry>) -> Manifest {
        Manifest {
            header: make_header(),
            entries,
        }
    }

    fn simple_entry(path: &str) -> Entry {
        Entry {
            path: path.into(),
            entry_type: EntryType::File,
            size: 42,
            blob: None,
            patch: None,
            metadata: None,
            hardlink_target: None,
            removed: false,
        }
    }

    fn simple_patch_ref(name: &str) -> PatchRef {
        PatchRef {
            archive_entry: name.into(),
            sha256: "ab".repeat(32),
            algorithm_code: AlgorithmCode::Xdelta3,
            algorithm_id: None,
        }
    }

    /// Serialise to MessagePack map format (field names included).
    ///
    /// `skip_serializing_if` only works correctly with map encoding because
    /// array encoding relies on field position — skipping a field shifts all
    /// subsequent positions, breaking deserialization.
    fn to_msgpack<T: serde::Serialize>(value: &T) -> Vec<u8> {
        rmp_serde::to_vec_named(value).unwrap()
    }

    fn from_msgpack<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
        rmp_serde::from_slice(bytes).unwrap()
    }

    fn assert_absent(json: &str, key: &str) {
        let needle = format!("\"{}\"", key);
        assert!(
            !json.contains(&needle),
            "field '{}' should be absent when None/false, found in: {}",
            key,
            json
        );
    }

    fn assert_present(json: &str, key: &str) {
        let needle = format!("\"{}\"", key);
        assert!(
            json.contains(&needle),
            "field '{}' should be present but not found in: {}",
            key,
            json
        );
    }

    // ── Round-trip tests ──────────────────────────────────────────────────────

    // 1. MessagePack round-trip preserves all fields.
    #[test]
    fn manifest_msgpack_roundtrip() {
        let original = make_manifest(vec![simple_entry("usr/bin/ls")]);

        let bytes = to_msgpack(&original);
        let recovered: Manifest = from_msgpack(&bytes);

        assert_eq!(recovered, original);
    }

    // 2. JSON round-trip preserves all fields.
    #[test]
    fn manifest_json_roundtrip() {
        let original = make_manifest(vec![simple_entry("etc/passwd")]);

        let json = serde_json::to_string(&original).unwrap();
        let recovered: Manifest = serde_json::from_str(&json).unwrap();

        assert_eq!(recovered, original);
    }

    // 3. Manifest with empty entries list serialises and deserialises cleanly.
    #[test]
    fn manifest_empty_entries_roundtrip() {
        let original = make_manifest(vec![]);

        let bytes = to_msgpack(&original);
        let recovered: Manifest = from_msgpack(&bytes);

        assert!(recovered.entries.is_empty());
    }

    // 4. EntryType serde names are snake_case (required for JSON interop).
    #[test]
    fn entry_type_serde_names() {
        let cases = [
            (EntryType::File, "\"file\""),
            (EntryType::Directory, "\"directory\""),
            (EntryType::Symlink, "\"symlink\""),
            (EntryType::Hardlink, "\"hardlink\""),
            (EntryType::Other, "\"other\""),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "wrong serde name for {variant:?}");
        }
    }

    // 5. Entry with BlobRef round-trips correctly.
    #[test]
    fn entry_with_blob_ref_roundtrip() {
        let blob_id = Uuid::new_v4();
        let mut entry = simple_entry("lib/libc.so.6");
        entry.blob = Some(BlobRef {
            blob_id,
            size: 2_000_000,
        });

        let bytes = to_msgpack(&entry);
        let recovered: Entry = from_msgpack(&bytes);

        let blob = recovered.blob.unwrap();
        assert_eq!(blob.blob_id, blob_id);
        assert_eq!(blob.size, 2_000_000);
    }

    // 6. Entry with PatchRef round-trips correctly.
    #[test]
    fn entry_with_patch_ref_roundtrip() {
        let mut entry = simple_entry("usr/lib/firmware.bin");
        entry.patch = Some(simple_patch_ref("usr_lib_firmware.bin.vcdiff"));

        let bytes = to_msgpack(&entry);
        let recovered: Entry = from_msgpack(&bytes);

        let patch = recovered.patch.unwrap();
        assert_eq!(patch.archive_entry, "usr_lib_firmware.bin.vcdiff");
        assert_eq!(patch.algorithm_code, AlgorithmCode::Xdelta3);
        assert_eq!(patch.algorithm_id, None);
        assert_eq!(patch.sha256, "ab".repeat(32));
    }

    // 7. Header without base_image_id round-trips with None preserved.
    #[test]
    fn header_without_base_image_roundtrip() {
        let header = ManifestHeader {
            version: MANIFEST_VERSION,
            image_id: "root-img".into(),
            base_image_id: None,
            format: "qcow2".into(),
            created_at: 0,
            patches_compressed: true,
        };

        let bytes = to_msgpack(&header);
        let recovered: ManifestHeader = from_msgpack(&bytes);

        assert_eq!(recovered, header);
        assert!(recovered.base_image_id.is_none());
        assert!(recovered.patches_compressed);
    }

    // 8. Header with base_image_id = Some(...) round-trips correctly.
    #[test]
    fn header_with_base_image_roundtrip() {
        let header = ManifestHeader {
            version: MANIFEST_VERSION,
            image_id: "child-img".into(),
            base_image_id: Some("parent-img".into()),
            format: "directory".into(),
            created_at: 42,
            patches_compressed: false,
        };

        let bytes = to_msgpack(&header);
        let recovered: ManifestHeader = from_msgpack(&bytes);

        assert_eq!(recovered, header);
        assert_eq!(recovered.base_image_id.as_deref(), Some("parent-img"));
    }

    // 9. Symlink entry: link_target stored inside Metadata.
    #[test]
    fn entry_symlink_roundtrip() {
        let mut entry = simple_entry("lib64");
        entry.entry_type = EntryType::Symlink;
        entry.size = 0;
        entry.metadata = Some(Metadata {
            link_target: Some("/lib".into()),
            ..Default::default()
        });

        let bytes = to_msgpack(&entry);
        let recovered: Entry = from_msgpack(&bytes);

        assert_eq!(recovered.entry_type, EntryType::Symlink);
        assert_eq!(
            recovered.metadata.unwrap().link_target.as_deref(),
            Some("/lib")
        );
    }

    // 10. Hardlink entry: hardlink_target field round-trips correctly.
    #[test]
    fn entry_hardlink_roundtrip() {
        let mut entry = simple_entry("usr/sbin/init");
        entry.entry_type = EntryType::Hardlink;
        entry.hardlink_target = Some("usr/lib/systemd/systemd".into());

        let bytes = to_msgpack(&entry);
        let recovered: Entry = from_msgpack(&bytes);

        assert_eq!(recovered.entry_type, EntryType::Hardlink);
        assert_eq!(
            recovered.hardlink_target.as_deref(),
            Some("usr/lib/systemd/systemd")
        );
    }

    // 11. MessagePack encoding is smaller than JSON for a realistic manifest.
    #[test]
    fn msgpack_is_smaller_than_json() {
        let mut entries: Vec<Entry> = Vec::new();

        // 5 plain files — all optional fields absent.
        for i in 0..5 {
            entries.push(simple_entry(&format!("usr/lib/libfoo{i}.so")));
        }

        // 5 files stored as verbatim blobs.
        for i in 0..5 {
            let mut e = simple_entry(&format!("usr/lib/libbar{i}.so"));
            e.blob = Some(BlobRef {
                blob_id: Uuid::new_v4(),
                size: 1024 * (i as u64 + 1),
            });
            entries.push(e);
        }

        // 5 files stored as patches inside the tar archive.
        for i in 0..5 {
            let mut e = simple_entry(&format!("usr/lib/libbaz{i}.so"));
            e.patch = Some(PatchRef {
                archive_entry: format!("usr_lib_libbaz{i}.vcdiff"),
                sha256: format!("{:064x}", i),
                algorithm_code: crate::AlgorithmCode::Xdelta3,
                algorithm_id: None,
            });
            entries.push(e);
        }

        // 4 directories.
        for dir in ["usr/lib", "usr/bin", "etc", "var/log"] {
            let mut e = simple_entry(dir);
            e.entry_type = EntryType::Directory;
            e.size = 0;
            entries.push(e);
        }

        // 4 symlinks — link_target lives inside Metadata.
        for (link, target) in [
            ("lib64", "/lib"),
            ("lib", "usr/lib"),
            ("bin", "usr/bin"),
            ("sbin", "usr/sbin"),
        ] {
            let mut e = simple_entry(link);
            e.entry_type = EntryType::Symlink;
            e.size = 0;
            e.metadata = Some(Metadata {
                link_target: Some(target.into()),
                ..Default::default()
            });
            entries.push(e);
        }

        // 4 hardlinks.
        for i in 0..4u8 {
            let mut e = simple_entry(&format!("usr/bin/tool{i}"));
            e.entry_type = EntryType::Hardlink;
            e.hardlink_target = Some(format!("usr/lib/tool{i}.real"));
            entries.push(e);
        }

        // 4 removed entries.
        for i in 0..4u8 {
            let mut e = simple_entry(&format!("usr/lib/old_lib{i}.so"));
            e.removed = true;
            entries.push(e);
        }

        let manifest = make_manifest(entries);

        let msgpack_bytes = to_msgpack(&manifest);
        let json_bytes = serde_json::to_vec(&manifest).unwrap();

        assert!(
            msgpack_bytes.len() < json_bytes.len(),
            "expected msgpack ({}) < json ({})",
            msgpack_bytes.len(),
            json_bytes.len()
        );

        // 5 plain + 5 blob + 5 patch + 4 dir + 4 symlink + 4 hardlink + 4 removed = 31.
        assert_eq!(manifest.entries.len(), 31);
    }

    // ── skip_serializing_if / default tests ───────────────────────────────────

    // 12. `patch` field: absent when None, present when Some.
    #[test]
    fn skip_none_patch() {
        let mut entry = simple_entry("lib/libm.so");
        assert!(entry.patch.is_none());

        let json_none = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_none, "patch");

        entry.patch = Some(simple_patch_ref("lib_libm.so.vcdiff"));
        let json_some = serde_json::to_string(&entry).unwrap();
        assert_present(&json_some, "patch");
    }

    // 13. `blob` field: absent when None, present when Some.
    #[test]
    fn skip_none_blob() {
        let mut entry = simple_entry("lib/libpthread.so");
        assert!(entry.blob.is_none());

        let json_none = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_none, "blob");

        entry.blob = Some(BlobRef {
            blob_id: Uuid::new_v4(),
            size: 131_072,
        });
        let json_some = serde_json::to_string(&entry).unwrap();
        assert_present(&json_some, "blob");
    }

    // 14. `metadata` field: absent when None, present when Some.
    #[test]
    fn skip_none_metadata() {
        let mut entry = simple_entry("etc/passwd");
        assert!(entry.metadata.is_none());

        let json_none = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_none, "metadata");

        entry.metadata = Some(Metadata {
            mode: Some(0o644),
            uid: Some(0),
            ..Default::default()
        });
        let json_some = serde_json::to_string(&entry).unwrap();
        assert_present(&json_some, "metadata");
    }

    // 15. `hardlink_target` field: absent when None, present when Some.
    #[test]
    fn skip_none_hardlink_target() {
        let mut entry = simple_entry("sbin/init");
        entry.entry_type = EntryType::Hardlink;
        assert!(entry.hardlink_target.is_none());

        let json_none = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_none, "hardlink_target");

        entry.hardlink_target = Some("usr/lib/systemd/systemd".into());
        let json_some = serde_json::to_string(&entry).unwrap();
        assert_present(&json_some, "hardlink_target");
    }

    // 16. `removed` field: absent when false, present when true.
    #[test]
    fn skip_false_removed() {
        let mut entry = simple_entry("usr/lib/libold.so");
        assert!(!entry.removed);

        let json_false = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_false, "removed");

        entry.removed = true;
        let json_true = serde_json::to_string(&entry).unwrap();
        assert_present(&json_true, "removed");
    }

    // 17. `base_image_id` in ManifestHeader: absent when None, present when Some.
    #[test]
    fn skip_none_base_image_id() {
        let header_none = ManifestHeader {
            version: MANIFEST_VERSION,
            image_id: "img-root".into(),
            base_image_id: None,
            format: "directory".into(),
            created_at: 0,
            patches_compressed: false,
        };
        let json_none = serde_json::to_string(&header_none).unwrap();
        assert_absent(&json_none, "base_image_id");

        let header_some = ManifestHeader {
            base_image_id: Some("img-base".into()),
            ..header_none
        };
        let json_some = serde_json::to_string(&header_some).unwrap();
        assert_present(&json_some, "base_image_id");
    }

    // 18. Metadata: None sub-fields are absent; only populated ones are present.
    #[test]
    fn metadata_sparse_serialization() {
        let meta = Metadata {
            mode: Some(0o755),
            uid: Some(1000),
            ..Default::default()
        };

        let json = serde_json::to_string(&meta).unwrap();
        assert_present(&json, "mode");
        assert_present(&json, "uid");
        assert_absent(&json, "gid");
        assert_absent(&json, "mtime");
        assert_absent(&json, "new_path");
        assert_absent(&json, "link_target");
        assert_absent(&json, "xattrs");

        // Round-trip preserves values.
        let recovered: Metadata = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.mode, Some(0o755));
        assert_eq!(recovered.uid, Some(1000));
        assert_eq!(recovered.gid, None);
    }

    // 19. ManifestHeader: version and patches_compressed round-trip correctly.
    #[test]
    fn header_version_and_patches_compressed() {
        let header = ManifestHeader {
            version: MANIFEST_VERSION,
            image_id: "test-img".into(),
            base_image_id: None,
            format: "directory".into(),
            created_at: 1_714_000_000,
            patches_compressed: true,
        };

        // msgpack round-trip
        let bytes = to_msgpack(&header);
        let recovered: ManifestHeader = from_msgpack(&bytes);
        assert_eq!(recovered.version, MANIFEST_VERSION);
        assert!(recovered.patches_compressed);

        // JSON: both fields must be present (they are not Option).
        let json = serde_json::to_string(&header).unwrap();
        assert_present(&json, "version");
        assert_present(&json, "patches_compressed");
    }

    // 20. Deserialising entries with no optional keys produces correct defaults.
    #[test]
    fn deserialize_missing_optional_fields_gives_defaults() {
        let json = serde_json::json!({
            "path": "etc/hostname",
            "entry_type": "file",
            "size": 8
        })
        .to_string();

        let entry: Entry = serde_json::from_str(&json).unwrap();

        assert!(entry.blob.is_none(), "blob should default to None");
        assert!(entry.patch.is_none(), "patch should default to None");
        assert!(entry.metadata.is_none(), "metadata should default to None");
        assert!(
            entry.hardlink_target.is_none(),
            "hardlink_target should default to None"
        );
        assert!(!entry.removed, "removed should default to false");
    }

    // 21. Minimal entry (no optional fields) is strictly smaller than a fully
    //     populated entry in msgpack.
    #[test]
    fn msgpack_none_fields_reduce_size() {
        let minimal = simple_entry("etc/os-release");

        let mut full = minimal.clone();
        full.blob = Some(BlobRef {
            blob_id: Uuid::new_v4(),
            size: 4096,
        });
        full.patch = Some(simple_patch_ref("etc_os-release.vcdiff"));
        full.hardlink_target = Some("/etc/os-release.hard".into());
        full.metadata = Some(Metadata {
            new_path: Some("/etc/os-release.real".into()),
            mode: Some(0o644),
            uid: Some(0),
            gid: Some(0),
            mtime: Some(1_714_000_000),
            link_target: Some("/etc/os-release.target".into()),
            xattrs: None,
        });

        let bytes_minimal = to_msgpack(&minimal);
        let bytes_full = to_msgpack(&full);

        assert!(
            bytes_minimal.len() < bytes_full.len(),
            "expected minimal ({} bytes) < full ({} bytes)",
            bytes_minimal.len(),
            bytes_full.len()
        );
    }
}
