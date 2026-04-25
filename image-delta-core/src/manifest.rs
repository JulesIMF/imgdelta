use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Serde helper: serialises `Option<[u8; 32]>` as a lowercase hex string in
/// human-readable formats (JSON) and as raw bytes (msgpack bin) in binary ones.
/// Combined with `skip_serializing_if = "Option::is_none"` and `default`,
/// the field is completely absent from the serialised form when `None`.
mod hex_sha256_opt {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(opt: &Option<[u8; 32]>, s: S) -> Result<S::Ok, S::Error> {
        match opt {
            None => s.serialize_none(),
            Some(bytes) => {
                if s.is_human_readable() {
                    s.serialize_some(&hex::encode(bytes))
                } else {
                    // msgpack bin format: compact, no per-byte overhead
                    s.serialize_some(serde_bytes::Bytes::new(bytes))
                }
            }
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 32]>, D::Error> {
        if d.is_human_readable() {
            let s = <Option<String>>::deserialize(d)?;
            match s {
                None => Ok(None),
                Some(hex_str) => {
                    let vec = hex::decode(&hex_str).map_err(D::Error::custom)?;
                    Ok(Some(vec.as_slice().try_into().map_err(|_| {
                        D::Error::custom(
                            "sha256 hex string must be exactly 64 characters (32 bytes)",
                        )
                    })?))
                }
            }
        } else {
            let b = <Option<serde_bytes::ByteBuf>>::deserialize(d)?;
            match b {
                None => Ok(None),
                Some(buf) => Ok(Some(buf.as_slice().try_into().map_err(|_| {
                    D::Error::custom("sha256 must be exactly 32 bytes in binary encoding")
                })?)),
            }
        }
    }
}

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
    #[serde(skip_serializing_if = "Option::is_none", default)]
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
    #[serde(
        with = "hex_sha256_opt",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub sha256: Option<[u8; 32]>,
    /// Unix permission bits.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Modification time as Unix timestamp (seconds).
    pub mtime: i64,
    /// Present when this file was stored as a delta against a base blob.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub delta: Option<DeltaRef>,
    /// Present when this file was stored as a verbatim blob (no base, or passthrough).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub blob: Option<BlobRef>,
    /// Symlink target, present when `entry_type == EntryType::Symlink`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link_target: Option<String>,
    /// Path of the first occurrence, present when `entry_type == EntryType::Hardlink`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
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

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build a minimal manifest with one file entry.
    fn make_manifest(entries: Vec<Entry>) -> Manifest {
        Manifest {
            header: ManifestHeader {
                image_id: "img-test-001".into(),
                base_image_id: Some("img-base-001".into()),
                format: "directory".into(),
                created_at: 1_714_000_000,
            },
            entries,
        }
    }

    fn simple_entry(path: &str) -> Entry {
        Entry {
            path: path.into(),
            entry_type: EntryType::File,
            size: 42,
            sha256: None,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            mtime: 1_714_000_000,
            delta: None,
            blob: None,
            link_target: None,
            hardlink_to: None,
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

    // 1. MessagePack round-trip preserves all fields.
    #[test]
    fn manifest_msgpack_roundtrip() {
        let original = make_manifest(vec![simple_entry("usr/bin/ls")]);

        let bytes = to_msgpack(&original);
        let recovered: Manifest = from_msgpack(&bytes);

        assert_eq!(recovered.header.image_id, original.header.image_id);
        assert_eq!(recovered.entries.len(), 1);
        assert_eq!(recovered.entries[0].path, "usr/bin/ls");
    }

    // 2. JSON round-trip preserves all fields.
    #[test]
    fn manifest_json_roundtrip() {
        let original = make_manifest(vec![simple_entry("etc/passwd")]);

        let json = serde_json::to_string(&original).unwrap();
        let recovered: Manifest = serde_json::from_str(&json).unwrap();

        assert_eq!(recovered.header.image_id, original.header.image_id);
        assert_eq!(recovered.entries[0].path, "etc/passwd");
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

    // 6. Entry with DeltaRef round-trips correctly.
    #[test]
    fn entry_with_delta_ref_roundtrip() {
        let blob_id = Uuid::new_v4();
        let base_blob_id = Uuid::new_v4();
        let mut entry = simple_entry("usr/lib/firmware.bin");
        entry.delta = Some(DeltaRef {
            blob_id,
            base_blob_id,
            algorithm: "xdelta3".into(),
            compressed_size: 1024,
        });

        let bytes = to_msgpack(&entry);
        let recovered: Entry = from_msgpack(&bytes);

        let delta = recovered.delta.unwrap();
        assert_eq!(delta.blob_id, blob_id);
        assert_eq!(delta.algorithm, "xdelta3");
        assert_eq!(delta.compressed_size, 1024);
    }

    // 7. Header with base_image_id = None.
    #[test]
    fn header_without_base_image_roundtrip() {
        let header = ManifestHeader {
            image_id: "root-img".into(),
            base_image_id: None,
            format: "qcow2".into(),
            created_at: 0,
        };

        let bytes = to_msgpack(&header);
        let recovered: ManifestHeader = from_msgpack(&bytes);

        assert!(recovered.base_image_id.is_none());
        assert_eq!(recovered.format, "qcow2");
    }

    // 8. Header with base_image_id = Some(...).
    #[test]
    fn header_with_base_image_roundtrip() {
        let header = ManifestHeader {
            image_id: "child-img".into(),
            base_image_id: Some("parent-img".into()),
            format: "directory".into(),
            created_at: 42,
        };

        let bytes = to_msgpack(&header);
        let recovered: ManifestHeader = from_msgpack(&bytes);

        assert_eq!(recovered.base_image_id.as_deref(), Some("parent-img"));
    }

    // 9. Symlink entry round-trips with link_target preserved.
    #[test]
    fn entry_symlink_roundtrip() {
        let mut entry = simple_entry("lib64");
        entry.entry_type = EntryType::Symlink;
        entry.link_target = Some("/lib".into());

        let bytes = to_msgpack(&entry);
        let recovered: Entry = from_msgpack(&bytes);

        assert_eq!(recovered.entry_type, EntryType::Symlink);
        assert_eq!(recovered.link_target.as_deref(), Some("/lib"));
    }

    // 10. Hardlink entry round-trips with hardlink_to preserved.
    #[test]
    fn entry_hardlink_roundtrip() {
        let mut entry = simple_entry("usr/sbin/init");
        entry.entry_type = EntryType::Hardlink;
        entry.hardlink_to = Some("usr/lib/systemd/systemd".into());

        let bytes = to_msgpack(&entry);
        let recovered: Entry = from_msgpack(&bytes);

        assert_eq!(recovered.entry_type, EntryType::Hardlink);
        assert_eq!(
            recovered.hardlink_to.as_deref(),
            Some("usr/lib/systemd/systemd")
        );
    }

    // 11. MessagePack encoding is smaller than JSON for a realistic manifest.
    //
    // The manifest contains a heterogeneous mix of entry types:
    //   - plain files (no optional fields)
    //   - files with sha256 + blob ref (verbatim storage)
    //   - files with sha256 + delta ref (delta storage)
    //   - directories
    //   - symlinks (with link_target)
    //   - hardlinks (with hardlink_to)
    //
    // This exercises the fact that `to_vec_named` (map encoding) omits keys
    // for None fields, keeping the msgpack payload smaller than the equivalent
    // JSON even with field-name strings included.
    #[test]
    fn msgpack_is_smaller_than_json() {
        let mut entries: Vec<Entry> = Vec::new();

        // 5 plain files — all optional fields absent.
        for i in 0..5 {
            entries.push(simple_entry(&format!("usr/lib/libfoo{i}.so")));
        }

        // 5 files stored as verbatim blobs (sha256 + blob ref).
        for i in 0..5 {
            let mut e = simple_entry(&format!("usr/lib/libbar{i}.so"));
            e.sha256 = Some([i as u8; 32]);
            e.blob = Some(BlobRef {
                blob_id: Uuid::new_v4(),
                size: 1024 * (i as u64 + 1),
            });
            entries.push(e);
        }

        // 5 files stored as deltas (sha256 + delta ref).
        for i in 0..5 {
            let mut e = simple_entry(&format!("usr/lib/libbaz{i}.so"));
            e.sha256 = Some([0x80 | i as u8; 32]);
            e.delta = Some(DeltaRef {
                blob_id: Uuid::new_v4(),
                base_blob_id: Uuid::new_v4(),
                algorithm: "xdelta3".into(),
                compressed_size: 256 * (i as u64 + 1),
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

        // 4 symlinks.
        for (link, target) in [
            ("lib64", "/lib"),
            ("lib", "usr/lib"),
            ("bin", "usr/bin"),
            ("sbin", "usr/sbin"),
        ] {
            let mut e = simple_entry(link);
            e.entry_type = EntryType::Symlink;
            e.size = 0;
            e.link_target = Some(target.into());
            entries.push(e);
        }

        // 4 hardlinks.
        for i in 0..4u8 {
            let mut e = simple_entry(&format!("usr/bin/tool{i}"));
            e.entry_type = EntryType::Hardlink;
            e.hardlink_to = Some(format!("usr/lib/tool{i}.real"));
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

        // Sanity: the manifest has the expected number of entries.
        // 5 plain + 5 blob + 5 delta + 4 dir + 4 symlink + 4 hardlink = 27.
        assert_eq!(manifest.entries.len(), 27);
    }

    // ── skip_serializing_if tests ─────────────────────────────────────────────
    // Strategy: serialise to JSON (which is human-readable) and assert that the
    // key is absent when the field is None and present when it is Some.
    // The same skip logic applies to MessagePack; we additionally check that the
    // msgpack bytes shrink when None fields are added.

    fn assert_absent(json: &str, key: &str) {
        let needle = format!("\"{}\"", key);
        assert!(
            !json.contains(&needle),
            "field '{}' should be absent when None, but found in: {}",
            key,
            json
        );
    }

    fn assert_present(json: &str, key: &str) {
        let needle = format!("\"{}\"", key);
        assert!(
            json.contains(&needle),
            "field '{}' should be present when Some, but not found in: {}",
            key,
            json
        );
    }

    // 12. sha256: absent when None, present when Some.
    //     In JSON: stored as a lowercase hex string (64 chars), not a byte array.
    //     In msgpack: stored as bin (raw 32 bytes), not a hex string.
    #[test]
    fn skip_none_sha256() {
        let mut entry = simple_entry("bin/ls");
        assert!(entry.sha256.is_none());

        let json_none = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_none, "sha256");

        entry.sha256 = Some([0xAB; 32]);

        // JSON: must be a 64-char hex string, not an array of integers.
        let json_some = serde_json::to_string(&entry).unwrap();
        assert_present(&json_some, "sha256");
        // Parse back as Value and inspect the sha256 field.
        let v: serde_json::Value = serde_json::from_str(&json_some).unwrap();
        let sha_val = &v["sha256"];
        assert!(
            sha_val.is_string(),
            "sha256 in JSON should be a string, got: {sha_val}"
        );
        let hex_str = sha_val.as_str().unwrap();
        assert_eq!(
            hex_str.len(),
            64,
            "sha256 hex string should be 64 chars (32 bytes), got {} chars: {hex_str}",
            hex_str.len()
        );
        assert!(
            hex_str.chars().all(|c| c.is_ascii_hexdigit()),
            "sha256 should only contain hex digits, got: {hex_str}"
        );
        assert_eq!(hex_str, "ab".repeat(32), "unexpected sha256 hex value");

        // msgpack: the field value must be raw 32 bytes (bin format), not a
        // 64-byte hex string.  We verify this by round-tripping and checking
        // that the total serialised size is nowhere near 64 extra bytes.
        let msgpack_some = to_msgpack(&entry);
        let recovered: Entry = from_msgpack(&msgpack_some);
        assert_eq!(recovered.sha256, Some([0xAB; 32]));

        // msgpack: field overhead = key "sha256" (7 bytes, fixstr) +
        //   bin8 header (2 bytes) + 32 payload bytes = 41 bytes.
        // A hex string encoding would cost 7 + 2 + 64 = 73 bytes.
        // Assert we're well below the hex threshold (< 50) to confirm bin format.
        let msgpack_none = to_msgpack(&simple_entry("bin/ls"));
        let sha256_overhead = msgpack_some.len() - msgpack_none.len();
        assert!(
            sha256_overhead < 50,
            "sha256 should cost < 50 bytes in msgpack (bin format = 41), got {sha256_overhead}; \
             hex format would cost ~73 bytes"
        );
    }

    // 13. delta: absent when None, present when Some.
    #[test]
    fn skip_none_delta() {
        let mut entry = simple_entry("lib/libm.so");
        assert!(entry.delta.is_none());

        let json_none = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_none, "delta");

        entry.delta = Some(DeltaRef {
            blob_id: Uuid::new_v4(),
            base_blob_id: Uuid::new_v4(),
            algorithm: "xdelta3".into(),
            compressed_size: 512,
        });
        let json_some = serde_json::to_string(&entry).unwrap();
        assert_present(&json_some, "delta");
    }

    // 14. blob: absent when None, present when Some.
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

    // 15. link_target: absent when None, present when Some.
    #[test]
    fn skip_none_link_target() {
        let mut entry = simple_entry("lib64");
        entry.entry_type = EntryType::Symlink;
        // link_target intentionally left None to test absence.

        let json_none = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_none, "link_target");

        entry.link_target = Some("/lib".into());
        let json_some = serde_json::to_string(&entry).unwrap();
        assert_present(&json_some, "link_target");
    }

    // 16. hardlink_to: absent when None, present when Some.
    #[test]
    fn skip_none_hardlink_to() {
        let mut entry = simple_entry("sbin/init");
        entry.entry_type = EntryType::Hardlink;
        // hardlink_to intentionally left None.

        let json_none = serde_json::to_string(&entry).unwrap();
        assert_absent(&json_none, "hardlink_to");

        entry.hardlink_to = Some("usr/lib/systemd/systemd".into());
        let json_some = serde_json::to_string(&entry).unwrap();
        assert_present(&json_some, "hardlink_to");
    }

    // 17. base_image_id in ManifestHeader: absent when None, present when Some.
    #[test]
    fn skip_none_base_image_id() {
        let header_none = ManifestHeader {
            image_id: "img-root".into(),
            base_image_id: None,
            format: "directory".into(),
            created_at: 0,
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

    // 18. Minimal entry (all Option fields None) produces strictly fewer msgpack
    //     bytes than an entry with all optional fields populated.
    #[test]
    fn msgpack_none_fields_reduce_size() {
        let minimal = simple_entry("etc/os-release");

        let mut full = minimal.clone();
        full.sha256 = Some([0x00; 32]);
        full.blob = Some(BlobRef {
            blob_id: Uuid::new_v4(),
            size: 4096,
        });
        full.delta = Some(DeltaRef {
            blob_id: Uuid::new_v4(),
            base_blob_id: Uuid::new_v4(),
            algorithm: "xdelta3".into(),
            compressed_size: 256,
        });
        full.link_target = Some("/etc/os-release.real".into());
        full.hardlink_to = Some("/etc/os-release.hard".into());

        let bytes_minimal = to_msgpack(&minimal);
        let bytes_full = to_msgpack(&full);

        assert!(
            bytes_minimal.len() < bytes_full.len(),
            "expected minimal ({} bytes) < full ({} bytes)",
            bytes_minimal.len(),
            bytes_full.len()
        );
    }

    // 19. Deserialising from JSON that has no optional keys produces None fields.
    //     (Validates that `#[serde(default)]` is set correctly.)
    #[test]
    fn deserialize_missing_optional_fields_gives_none() {
        // Manually craft minimal JSON with no optional keys.
        let json = serde_json::json!({
            "path": "etc/hostname",
            "entry_type": "file",
            "size": 8,
            "mode": 0o644u32,
            "uid": 0,
            "gid": 0,
            "mtime": 0
        })
        .to_string();

        let entry: Entry = serde_json::from_str(&json).unwrap();

        assert!(entry.sha256.is_none(), "sha256 should default to None");
        assert!(entry.delta.is_none(), "delta should default to None");
        assert!(entry.blob.is_none(), "blob should default to None");
        assert!(
            entry.link_target.is_none(),
            "link_target should default to None"
        );
        assert!(
            entry.hardlink_to.is_none(),
            "hardlink_to should default to None"
        );
    }

    // 20. Deserialising a header with no base_image_id key gives None.
    #[test]
    fn deserialize_missing_base_image_id_gives_none() {
        let json = serde_json::json!({
            "image_id": "img-root",
            "format": "directory",
            "created_at": 0u64
        })
        .to_string();

        let header: ManifestHeader = serde_json::from_str(&json).unwrap();
        assert!(header.base_image_id.is_none());
    }
}
