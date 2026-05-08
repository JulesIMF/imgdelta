// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// BiosBootHandle — handle for raw BIOS-boot partitions

use super::PartitionDescriptor;

type WriteFn = Box<dyn Fn(&[u8]) -> crate::Result<()> + Send + Sync>;

/// Handle to a BIOS-boot (raw bytes) partition.
pub struct BiosBootHandle {
    /// Partition descriptor from the image's partition table.
    pub descriptor: PartitionDescriptor,
    read_fn: Box<dyn Fn() -> crate::Result<Vec<u8>> + Send + Sync>,
    write_fn: Option<WriteFn>,
}

impl BiosBootHandle {
    /// Create a new read-only [`BiosBootHandle`].
    pub fn new(
        descriptor: PartitionDescriptor,
        read_fn: impl Fn() -> crate::Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            read_fn: Box::new(read_fn),
            write_fn: None,
        }
    }

    /// Create a new read-write [`BiosBootHandle`].
    pub fn new_rw(
        descriptor: PartitionDescriptor,
        read_fn: impl Fn() -> crate::Result<Vec<u8>> + Send + Sync + 'static,
        write_fn: impl Fn(&[u8]) -> crate::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            read_fn: Box::new(read_fn),
            write_fn: Some(Box::new(write_fn)),
        }
    }

    /// Read the raw bytes of this partition.
    pub fn read_raw(&self) -> crate::Result<Vec<u8>> {
        (self.read_fn)()
    }

    /// Write raw bytes to this partition.
    ///
    /// Returns [`crate::Error::Format`] if this handle was opened read-only.
    pub fn write_raw(&self, data: &[u8]) -> crate::Result<()> {
        match &self.write_fn {
            Some(f) => f(data),
            None => Err(crate::Error::Format("BiosBootHandle is read-only".into())),
        }
    }
}
