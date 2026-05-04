// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Manifest / PartitionManifest / PartitionContent serialization types (MessagePack v2)

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::partition::{DiskLayout, PartitionDescriptor};
use crate::AlgorithmCode;

/// Manifest format version stored in every [`ManifestHeader`].
///
/// Increment this constant on every breaking schema change so that older
/// decompressors can refuse to process manifests they don't understand.
pub const MANIFEST_VERSION: u32 = 2;

/// Top-level manifest produced by a compress operation.
/// Serialised as MessagePack for storage; JSON for debugging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub header: ManifestHeader,
    pub disk_layout: DiskLayout,
    pub partitions: Vec<PartitionManifest>,
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
    pub patches_compressed: bool,
}

/// Reference to an entry inside the `patches.tar[.gz]` archive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PatchRef {
    /// Name of the member file inside the patches tar archive.
    pub archive_entry: String,
    /// Lowercase hex SHA-256 of the patch bytes — verified after extraction.
    pub sha256: String,
    /// Compact one-byte algorithm code — primary decoder lookup key.
    pub algorithm_code: AlgorithmCode,
    /// Human-readable algorithm identifier.
    /// `None` for built-in algorithms; `Some` only when
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
/// Only the fields that actually changed are populated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Metadata {
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
    /// Extended attributes (e.g. security capabilities).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub xattrs: Option<HashMap<String, Vec<u8>>>,
}

/// Reference to a data source used during the compress pipeline.
///
/// `FilePath` is transient and must never appear in a serialised manifest.
/// `BlobRef` is stable and safe to serialise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum DataRef {
    /// Path to a file on the local filesystem (compress pipeline only).
    FilePath(PathBuf),
    /// Reference to a blob already stored in S3.
    BlobRef(BlobRef),
}

/// Content data associated with a [`Record`] entry.
///
/// Variants containing `PathBuf` (`LazyBlob`, `OriginalFile`) are transient
/// and are resolved during the compress pipeline before serialisation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Data {
    /// Blob already stored in S3.
    BlobRef(BlobRef),
    /// File on the target mount that needs to be uploaded.
    LazyBlob(PathBuf),
    /// File on the base mount whose blob is already in S3 from a previous
    /// compress run.  Used as the delta source for deleted/renamed files.
    OriginalFile(PathBuf),
    /// Symlink target — for newly added symlinks.
    SoftlinkTo(String),
    /// Hardlink canonical target path — for newly added hardlinks.
    HardlinkTo(String),
}

/// Patch descriptor for a [`Record`] entry.
///
/// `Lazy` is transient (compress pipeline only) and must not appear in
/// serialised manifests.  After `compute_patches`, all patches become `Real`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Patch {
    /// Finalised patch stored as an entry in the patches archive.
    Real(PatchRef),
    /// Deferred patch — source and target data not yet encoded.
    Lazy {
        old_data: DataRef,
        new_data: DataRef,
    },
}

/// One changed path in a filesystem partition.
///
/// Unchanged files are **not** recorded; they are taken from the base image
/// during decompression.
///
/// # Path semantics
///
/// | `old_path`  | `new_path`  | Meaning                         |
/// |-------------|-------------|---------------------------------|
/// | `None`      | `Some(p)`   | File added at `p`               |
/// | `Some(p)`   | `None`      | File at `p` deleted             |
/// | `Some(p)`   | `Some(p)`   | File at `p` changed in-place    |
/// | `Some(old)` | `Some(new)` | File renamed `old` → `new`      |
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    /// Path in the base image, or `None` for newly added files.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub old_path: Option<String>,
    /// Path in the target image, or `None` for deleted files.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub new_path: Option<String>,
    pub entry_type: EntryType,
    /// Uncompressed size in bytes.  Zero for directories, symlinks, and deletions.
    pub size: u64,
    /// Content data.  `None` for metadata-only changes and deletions.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<Data>,
    /// Binary patch.  `None` for blob-only entries and deletions.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub patch: Option<Patch>,
    /// Changed filesystem attributes.  `None` when no attributes changed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metadata: Option<Metadata>,
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

/// Manifest for a single partition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartitionManifest {
    pub descriptor: PartitionDescriptor,
    pub content: PartitionContent,
}

/// Content encoding for one partition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PartitionContent {
    /// BIOS Boot partition — stored as a single verbatim blob (SHA-256 dedup).
    BiosBoot {
        blob_id: Uuid,
        sha256: String,
        size: u64,
    },
    /// Raw binary partition — blob dedup or xdelta3 on the whole partition.
    Raw {
        size: u64,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        blob: Option<BlobRef>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        patch: Option<PatchRef>,
    },
    /// Filesystem partition — file-level delta records.
    Fs {
        /// Filesystem type, e.g. `"ext4"`, `"vfat"`.
        fs_type: String,
        records: Vec<Record>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_msgpack<T: serde::Serialize>(value: &T) -> Vec<u8> {
        rmp_serde::to_vec_named(value).unwrap()
    }

    fn from_msgpack<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
        rmp_serde::from_slice(bytes).unwrap()
    }

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

    fn simple_blob_ref() -> BlobRef {
        BlobRef {
            blob_id: Uuid::nil(),
            size: 1024,
        }
    }

    fn simple_patch_ref_real() -> Patch {
        Patch::Real(PatchRef {
            archive_entry: "abc123.patch".into(),
            sha256: "de".repeat(32),
            algorithm_code: AlgorithmCode::Xdelta3,
            algorithm_id: None,
        })
    }

    // ── ManifestHeader ────────────────────────────────────────────────────────

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
    }

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
    }

    #[test]
    fn header_base_image_id_absent_in_json_when_none() {
        let header = ManifestHeader {
            version: MANIFEST_VERSION,
            image_id: "root-img".into(),
            base_image_id: None,
            format: "directory".into(),
            created_at: 0,
            patches_compressed: false,
        };
        let json = serde_json::to_string(&header).unwrap();
        assert!(
            !json.contains("\"base_image_id\""),
            "should be absent: {json}"
        );
    }

    // ── Metadata ──────────────────────────────────────────────────────────────

    #[test]
    fn metadata_sparse_serialization() {
        let meta = Metadata {
            mode: Some(0o755),
            uid: Some(1000),
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"mode\""));
        assert!(json.contains("\"uid\""));
        assert!(!json.contains("\"gid\""));
        assert!(!json.contains("\"mtime\""));
        assert!(!json.contains("\"xattrs\""));
        let recovered: Metadata = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.mode, Some(0o755));
        assert_eq!(recovered.uid, Some(1000));
        assert_eq!(recovered.gid, None);
    }

    #[test]
    fn metadata_msgpack_roundtrip() {
        let meta = Metadata {
            mode: Some(0o644),
            uid: Some(0),
            gid: Some(0),
            mtime: Some(1_714_000_000),
            xattrs: None,
        };
        let bytes = to_msgpack(&meta);
        let recovered: Metadata = from_msgpack(&bytes);
        assert_eq!(meta, recovered);
    }

    // ── DataRef ───────────────────────────────────────────────────────────────

    #[test]
    fn data_ref_file_path_roundtrip() {
        let dr = DataRef::FilePath("/mnt/target/usr/bin/bash".into());
        let json = serde_json::to_string(&dr).unwrap();
        let recovered: DataRef = serde_json::from_str(&json).unwrap();
        assert_eq!(dr, recovered);
    }

    #[test]
    fn data_ref_blob_ref_roundtrip() {
        let dr = DataRef::BlobRef(simple_blob_ref());
        let bytes = to_msgpack(&dr);
        let recovered: DataRef = from_msgpack(&bytes);
        assert_eq!(dr, recovered);
    }

    // ── Data ──────────────────────────────────────────────────────────────────

    #[test]
    fn data_blob_ref_roundtrip() {
        let data = Data::BlobRef(simple_blob_ref());
        let bytes = to_msgpack(&data);
        assert_eq!(from_msgpack::<Data>(&bytes), data);
    }

    #[test]
    fn data_lazy_blob_roundtrip() {
        let data = Data::LazyBlob("/mnt/target/usr/lib/libz.so.1".into());
        let bytes = to_msgpack(&data);
        assert_eq!(from_msgpack::<Data>(&bytes), data);
    }

    #[test]
    fn data_original_file_roundtrip() {
        let data = Data::OriginalFile("/mnt/base/etc/passwd".into());
        let bytes = to_msgpack(&data);
        assert_eq!(from_msgpack::<Data>(&bytes), data);
    }

    #[test]
    fn data_softlink_to_roundtrip() {
        let data = Data::SoftlinkTo("/usr/bin/python3".into());
        let bytes = to_msgpack(&data);
        assert_eq!(from_msgpack::<Data>(&bytes), data);
    }

    #[test]
    fn data_hardlink_to_roundtrip() {
        let data = Data::HardlinkTo("usr/share/common-licenses/GPL-2".into());
        let bytes = to_msgpack(&data);
        assert_eq!(from_msgpack::<Data>(&bytes), data);
    }

    // ── Patch ─────────────────────────────────────────────────────────────────

    #[test]
    fn patch_real_roundtrip() {
        let p = simple_patch_ref_real();
        let bytes = to_msgpack(&p);
        assert_eq!(from_msgpack::<Patch>(&bytes), p);
    }

    #[test]
    fn patch_lazy_roundtrip() {
        let p = Patch::Lazy {
            old_data: DataRef::FilePath("/mnt/base/usr/bin/bash".into()),
            new_data: DataRef::FilePath("/mnt/target/usr/bin/bash".into()),
        };
        let bytes = to_msgpack(&p);
        assert_eq!(from_msgpack::<Patch>(&bytes), p);
    }

    #[test]
    fn patch_lazy_blob_ref_roundtrip() {
        let p = Patch::Lazy {
            old_data: DataRef::BlobRef(simple_blob_ref()),
            new_data: DataRef::FilePath("/mnt/target/usr/lib/libz.so.2".into()),
        };
        let bytes = to_msgpack(&p);
        assert_eq!(from_msgpack::<Patch>(&bytes), p);
    }

    // ── Record ────────────────────────────────────────────────────────────────

    fn make_record_added() -> Record {
        Record {
            old_path: None,
            new_path: Some("usr/bin/newcmd".into()),
            entry_type: EntryType::File,
            size: 4096,
            data: Some(Data::BlobRef(simple_blob_ref())),
            patch: None,
            metadata: None,
        }
    }

    fn make_record_deleted() -> Record {
        Record {
            old_path: Some("usr/bin/oldcmd".into()),
            new_path: None,
            entry_type: EntryType::File,
            size: 0,
            data: None,
            patch: None,
            metadata: None,
        }
    }

    fn make_record_changed() -> Record {
        Record {
            old_path: Some("etc/config.conf".into()),
            new_path: Some("etc/config.conf".into()),
            entry_type: EntryType::File,
            size: 512,
            data: None,
            patch: Some(simple_patch_ref_real()),
            metadata: None,
        }
    }

    fn make_record_renamed() -> Record {
        Record {
            old_path: Some("lib/libfoo.so.1".into()),
            new_path: Some("lib/libfoo.so.2".into()),
            entry_type: EntryType::File,
            size: 8192,
            data: None,
            patch: Some(simple_patch_ref_real()),
            metadata: Some(Metadata::default()),
        }
    }

    fn make_record_symlink_added() -> Record {
        Record {
            old_path: None,
            new_path: Some("usr/bin/python".into()),
            entry_type: EntryType::Symlink,
            size: 0,
            data: Some(Data::SoftlinkTo("/usr/bin/python3.11".into())),
            patch: None,
            metadata: None,
        }
    }

    fn make_record_dir_metadata_only() -> Record {
        Record {
            old_path: Some("etc/ssl".into()),
            new_path: Some("etc/ssl".into()),
            entry_type: EntryType::Directory,
            size: 0,
            data: None,
            patch: None,
            metadata: Some(Metadata {
                mode: Some(0o755),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn record_added_msgpack_roundtrip() {
        let r = make_record_added();
        let bytes = to_msgpack(&r);
        assert_eq!(from_msgpack::<Record>(&bytes), r);
    }

    #[test]
    fn record_deleted_msgpack_roundtrip() {
        let r = make_record_deleted();
        let bytes = to_msgpack(&r);
        assert_eq!(from_msgpack::<Record>(&bytes), r);
    }

    #[test]
    fn record_changed_msgpack_roundtrip() {
        let r = make_record_changed();
        let bytes = to_msgpack(&r);
        assert_eq!(from_msgpack::<Record>(&bytes), r);
    }

    #[test]
    fn record_renamed_msgpack_roundtrip() {
        let r = make_record_renamed();
        let bytes = to_msgpack(&r);
        assert_eq!(from_msgpack::<Record>(&bytes), r);
    }

    #[test]
    fn record_symlink_added_roundtrip() {
        let r = make_record_symlink_added();
        let bytes = to_msgpack(&r);
        assert_eq!(from_msgpack::<Record>(&bytes), r);
    }

    #[test]
    fn record_dir_metadata_only_roundtrip() {
        let r = make_record_dir_metadata_only();
        let bytes = to_msgpack(&r);
        assert_eq!(from_msgpack::<Record>(&bytes), r);
    }

    #[test]
    fn record_optional_fields_absent_when_none() {
        let r = make_record_deleted();
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("\"new_path\":null"), "{json}");
        assert!(!json.contains("\"data\":null"), "{json}");
        assert!(!json.contains("\"patch\":null"), "{json}");
        assert!(!json.contains("\"metadata\":null"), "{json}");
    }

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

    // ── PartitionContent ──────────────────────────────────────────────────────

    #[test]
    fn partition_content_bios_boot_roundtrip() {
        let c = PartitionContent::BiosBoot {
            blob_id: Uuid::nil(),
            sha256: "ab".repeat(32),
            size: 1_048_576,
        };
        let bytes = to_msgpack(&c);
        assert_eq!(from_msgpack::<PartitionContent>(&bytes), c);
    }

    #[test]
    fn partition_content_raw_roundtrip() {
        let c = PartitionContent::Raw {
            size: 1_073_741_824,
            blob: Some(simple_blob_ref()),
            patch: None,
        };
        let bytes = to_msgpack(&c);
        assert_eq!(from_msgpack::<PartitionContent>(&bytes), c);
    }

    #[test]
    fn partition_content_fs_roundtrip() {
        let c = PartitionContent::Fs {
            fs_type: "ext4".into(),
            records: vec![
                make_record_added(),
                make_record_deleted(),
                make_record_changed(),
            ],
        };
        let bytes = to_msgpack(&c);
        assert_eq!(from_msgpack::<PartitionContent>(&bytes), c);
    }

    // ── Manifest ──────────────────────────────────────────────────────────────

    #[test]
    fn manifest_msgpack_roundtrip() {
        use crate::partition::{DiskLayout, DiskScheme, PartitionDescriptor, PartitionKind};
        let manifest = Manifest {
            header: make_header(),
            disk_layout: DiskLayout {
                scheme: DiskScheme::SingleFs,
                disk_guid: None,
                partitions: vec![],
            },
            partitions: vec![PartitionManifest {
                descriptor: PartitionDescriptor {
                    number: 1,
                    partition_guid: None,
                    type_guid: None,
                    name: None,
                    start_lba: 0,
                    end_lba: 0,
                    size_bytes: 0,
                    flags: 0,
                    kind: PartitionKind::Fs {
                        fs_type: "ext4".into(),
                    },
                },
                content: PartitionContent::Fs {
                    fs_type: "ext4".into(),
                    records: vec![make_record_added()],
                },
            }],
        };
        let bytes = to_msgpack(&manifest);
        let recovered: Manifest = from_msgpack(&bytes);
        assert_eq!(recovered, manifest);
    }
}
