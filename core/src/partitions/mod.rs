// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// partitions — disk layout types and per-partition handle types

//! Disk layout types and per-partition handle types.
//!
//! Describes the partition table and per-partition metadata of a disk image.
//! Used in [`Manifest`] to encode the disk layout alongside per-partition
//! content deltas.
//!
//! [`Manifest`]: crate::manifest::Manifest

pub mod bios_boot;
pub mod fs;
pub mod mbr;
pub mod raw;

pub use bios_boot::BiosBootHandle;
pub use fs::{FsHandle, MountHandle, SimpleMountHandle};
pub use mbr::MbrHandle;
pub use raw::RawHandle;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── DiskScheme ────────────────────────────────────────────────────────────────

/// Partitioning scheme of a disk image.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiskScheme {
    /// GUID Partition Table (64-bit LBA, modern).
    Gpt,
    /// Master Boot Record (32-bit LBA, legacy).
    Mbr,
    /// No partition table — the image is a raw filesystem or a directory tree.
    SingleFs,
}

// ── PartitionKind ─────────────────────────────────────────────────────────────

/// Semantic role of a single partition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum PartitionKind {
    /// BIOS Boot partition (GPT type GUID `21686148-…`) — raw binary, no
    /// filesystem.  Stored as a single blob via SHA-256 dedup.
    BiosBoot,
    /// Mountable filesystem partition.  File-level delta is applied.
    Fs {
        /// Filesystem type string as reported by `blkid`, e.g. `"ext4"`,
        /// `"xfs"`, `"vfat"`.
        fs_type: String,
    },
    /// Unrecognised or opaque partition — treated as a raw binary blob.
    /// Compressed via xdelta3 on the whole partition as a single "file".
    Raw,
    /// Synthetic region representing the MBR boot-code area (bytes 0–439
    /// of the raw disk, before the partition table at offset 446).
    ///
    /// Not a real partition-table entry; represented as partition number 0.
    /// Captures the GRUB stage-1 jump stub so it can be restored after
    /// `sgdisk --clear` rewrites LBA 0 during decompression.
    MbrBootCode,
}

// ── PartitionDescriptor ───────────────────────────────────────────────────────

/// Describes a single partition in the disk image.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartitionDescriptor {
    /// 1-based partition number as reported by the partition table.
    pub number: u32,

    /// GPT partition GUID (unique per partition instance).
    /// `None` for MBR partitions.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub partition_guid: Option<Uuid>,

    /// GPT type GUID (identifies the role: EFI System, Linux Data, …).
    /// `None` for MBR partitions.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub type_guid: Option<Uuid>,

    /// Human-readable partition label (decoded from UTF-16 in the GPT entry).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,

    /// First LBA of the partition (inclusive).
    pub start_lba: u64,

    /// Last LBA of the partition (inclusive).
    pub end_lba: u64,

    /// Partition size in bytes.
    pub size_bytes: u64,

    /// GPT attribute flags bitmask (0 for most partitions).
    pub flags: u64,

    /// Semantic role, derived from `type_guid` and a `blkid` probe.
    pub kind: PartitionKind,
}

// ── DiskLayout ────────────────────────────────────────────────────────────────

/// Full disk layout of a qcow2 or raw disk image.
///
/// Captured at compress time by reading the partition table.  Stored in the
/// [`Manifest`] so that `pack()` can reconstruct the exact disk geometry.
///
/// [`Manifest`]: crate::manifest::Manifest
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskLayout {
    /// Partitioning scheme.
    pub scheme: DiskScheme,

    /// GPT disk GUID.  `None` for MBR and `SingleFs`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub disk_guid: Option<Uuid>,

    /// Ordered list of partitions, sorted by `number`.
    /// Does not include the synthetic MBR boot-code entry (number 0),
    /// which lives in [`Manifest::partitions`] as [`PartitionKind::MbrBootCode`].
    pub partitions: Vec<PartitionDescriptor>,
}

// ── PartitionHandle ───────────────────────────────────────────────────────────

/// A partition accessible through an open image, in one of three forms.
pub enum PartitionHandle {
    /// BIOS-boot (raw bytes, e.g. GRUB stage 1.5).
    BiosBoot(BiosBootHandle),
    /// Formatted filesystem partition.
    Fs(FsHandle),
    /// Unformatted raw partition.
    Raw(RawHandle),
    /// MBR boot-code area (bytes 0–439, before partition table at offset 446).
    /// Synthetic partition number 0; not a real partition-table entry.
    Mbr(MbrHandle),
}

impl PartitionHandle {
    /// Return the [`PartitionDescriptor`] regardless of variant.
    pub fn descriptor(&self) -> &PartitionDescriptor {
        match self {
            Self::BiosBoot(h) => &h.descriptor,
            Self::Fs(h) => &h.descriptor,
            Self::Raw(h) => &h.descriptor,
            Self::Mbr(h) => &h.descriptor,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn gpt_layout() -> DiskLayout {
        DiskLayout {
            scheme: DiskScheme::Gpt,
            disk_guid: Some(Uuid::nil()),
            partitions: vec![
                PartitionDescriptor {
                    number: 1,
                    partition_guid: Some(Uuid::nil()),
                    type_guid: Some(
                        "21686148-6449-6e6f-744e-656564454649"
                            .parse()
                            .expect("valid UUID"),
                    ),
                    name: Some("BIOS boot".into()),
                    start_lba: 2048,
                    end_lba: 4095,
                    size_bytes: 1_048_576,
                    flags: 0,
                    kind: PartitionKind::BiosBoot,
                },
                PartitionDescriptor {
                    number: 2,
                    partition_guid: None,
                    type_guid: None,
                    name: None,
                    start_lba: 4096,
                    end_lba: 20_000_000,
                    size_bytes: 10_000_000_000,
                    flags: 0,
                    kind: PartitionKind::Fs {
                        fs_type: "ext4".into(),
                    },
                },
            ],
        }
    }

    fn to_msgpack<T: serde::Serialize>(value: &T) -> Vec<u8> {
        rmp_serde::to_vec_named(value).unwrap()
    }

    fn from_msgpack<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
        rmp_serde::from_slice(bytes).unwrap()
    }

    #[test]
    fn disk_layout_msgpack_roundtrip() {
        let original = gpt_layout();
        let bytes = to_msgpack(&original);
        let recovered: DiskLayout = from_msgpack(&bytes);
        assert_eq!(original, recovered);
    }

    #[test]
    fn disk_scheme_json_roundtrip() {
        for (scheme, expected) in [
            (DiskScheme::Gpt, r#""gpt""#),
            (DiskScheme::Mbr, r#""mbr""#),
            (DiskScheme::SingleFs, r#""single_fs""#),
        ] {
            let json = serde_json::to_string(&scheme).unwrap();
            assert_eq!(json, expected);
            let recovered: DiskScheme = serde_json::from_str(&json).unwrap();
            assert_eq!(recovered, scheme);
        }
    }

    #[test]
    fn partition_kind_json_roundtrip() {
        let kinds = vec![
            PartitionKind::BiosBoot,
            PartitionKind::Fs {
                fs_type: "vfat".into(),
            },
            PartitionKind::Raw,
            PartitionKind::MbrBootCode,
        ];
        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            let recovered: PartitionKind = serde_json::from_str(&json).unwrap();
            assert_eq!(recovered, kind);
        }
    }

    #[test]
    fn partition_descriptor_optional_fields_absent_when_none() {
        let desc = PartitionDescriptor {
            number: 1,
            partition_guid: None,
            type_guid: None,
            name: None,
            start_lba: 2048,
            end_lba: 4095,
            size_bytes: 1_048_576,
            flags: 0,
            kind: PartitionKind::Raw,
        };
        let json = serde_json::to_string(&desc).unwrap();
        assert!(!json.contains("\"partition_guid\""));
        assert!(!json.contains("\"type_guid\""));
        assert!(!json.contains("\"name\""));
    }

    #[test]
    fn single_fs_layout_roundtrip() {
        let layout = DiskLayout {
            scheme: DiskScheme::SingleFs,
            disk_guid: None,
            partitions: vec![],
        };
        let bytes = to_msgpack(&layout);
        let recovered: DiskLayout = from_msgpack(&bytes);
        assert_eq!(layout, recovered);
    }
}
