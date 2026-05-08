// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// DirectoryImage: treats a plain directory tree as an image with one FS partition

use crate::{
    image::OpenImage, partition::DiskLayout, partitions::SimpleMountHandle, DiskScheme, FsHandle,
    Image, MountHandle, PartitionContent, PartitionDescriptor, PartitionHandle, PartitionManifest,
    Result,
};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// [`Image`] implementation for a plain directory.
///
/// "Mounting" is a no-op: the directory is used as-is.  No external tools
/// required.  This is the format used for all L1 and L2 tests, and for
/// providers whose pipeline already extracts images to directories.
pub struct DirectoryImage;

impl DirectoryImage {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DirectoryImage {
    fn default() -> Self {
        Self::new()
    }
}

impl Image for DirectoryImage {
    fn format_name(&self) -> &'static str {
        "directory"
    }

    fn open(&self, path: &Path) -> Result<Box<dyn OpenImage>> {
        if !path.is_dir() {
            return Err(crate::Error::Format(format!(
                "DirectoryImage::open: path is not a directory: {}",
                path.display()
            )));
        }
        Ok(Box::new(OpenDirectory::new(path.to_path_buf())))
    }

    fn mount(&self, path: &Path) -> Result<Box<dyn MountHandle>> {
        Ok(Box::new(SimpleMountHandle::new(path.to_path_buf())))
    }

    fn pack(&self, source_dir: &Path, output_path: &Path) -> Result<()> {
        if output_path.exists() {
            std::fs::remove_dir_all(output_path)
                .map_err(|e| crate::Error::Format(e.to_string()))?;
        }
        std::fs::create_dir_all(output_path).map_err(|e| crate::Error::Format(e.to_string()))?;
        for entry in WalkDir::new(source_dir).min_depth(1) {
            let entry = entry.map_err(|e| crate::Error::Format(e.to_string()))?;
            let rel = entry
                .path()
                .strip_prefix(source_dir)
                .map_err(|e| crate::Error::Format(e.to_string()))?;
            let dest = output_path.join(rel);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&dest).map_err(|e| crate::Error::Format(e.to_string()))?;
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| crate::Error::Format(e.to_string()))?;
                }
                std::fs::copy(entry.path(), &dest)
                    .map_err(|e| crate::Error::Format(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Create a new, writable directory image at `path`.
    ///
    /// Creates the directory (and any parent directories) and returns an
    /// [`OpenDirectory`] whose [`create_partition`][crate::image::OpenImage::create_partition]
    /// writes partition data as files into that directory.
    fn create(&self, path: &Path, _layout: &DiskLayout) -> Result<Box<dyn OpenImage>> {
        std::fs::create_dir_all(path)
            .map_err(|e| crate::Error::Format(format!("create_dir_all {}: {e}", path.display())))?;
        Ok(Box::new(OpenDirectory::new(path.to_path_buf())))
    }
}

// ── OpenDirectory (DirectoryImage OpenImage impl) ────────────────────────────

/// [`OpenImage`] returned by `DirectoryImage::open()`.
///
/// The directory itself is the single filesystem partition; no actual mounting
/// is performed.
pub struct OpenDirectory {
    path: PathBuf,
    layout: DiskLayout,
}

impl OpenDirectory {
    fn new(path: PathBuf) -> Self {
        let layout = DiskLayout {
            scheme: DiskScheme::SingleFs,
            disk_guid: None,
            partitions: vec![],
        };
        Self { path, layout }
    }
}

impl OpenImage for OpenDirectory {
    fn disk_layout(&self) -> &DiskLayout {
        &self.layout
    }

    fn partitions(&self) -> crate::Result<Vec<PartitionHandle>> {
        use crate::partition::PartitionKind;

        let descriptor = PartitionDescriptor {
            number: 1,
            partition_guid: None,
            type_guid: None,
            name: Some("root".into()),
            start_lba: 0,
            end_lba: 0,
            size_bytes: 0,
            flags: 0,
            kind: PartitionKind::Fs {
                fs_type: "directory".into(),
            },
        };

        let path = self.path.clone();
        let handle = FsHandle::new(descriptor, move || {
            Ok(Box::new(SimpleMountHandle::new(path.clone())))
        });

        Ok(vec![PartitionHandle::Fs(handle)])
    }

    /// Create a writable partition handle that writes into this directory.
    ///
    /// - `Fs`                        → [`SimpleMountHandle`] wrapping the directory root.
    /// - others (BiosBoot, Raw, Mbr) → error, since they don't make sense for a directory.
    fn create_partition(&self, pm: &PartitionManifest) -> crate::Result<PartitionHandle> {
        let desc = pm.descriptor.clone();
        match &pm.content {
            PartitionContent::Fs { .. } => {
                let path = self.path.clone();
                Ok(PartitionHandle::Fs(FsHandle::new(desc, move || {
                    Ok(Box::new(SimpleMountHandle::new(path.clone())) as Box<dyn MountHandle>)
                })))
            }
            _ => Err(crate::Error::Format(format!(
                "OpenDirectory can only create Fs partitions, got kind {:?}",
                desc.kind
            ))),
        }
    }
}
