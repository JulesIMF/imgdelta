// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// BiosBootHandle — handle for raw BIOS-boot partitions

use super::PartitionDescriptor;

/// Handle to a BIOS-boot (raw bytes) partition.
pub struct BiosBootHandle {
    /// Partition descriptor from the image's partition table.
    pub descriptor: PartitionDescriptor,
    read_fn: Box<dyn Fn() -> crate::Result<Vec<u8>> + Send + Sync>,
}

impl BiosBootHandle {
    /// Create a new [`BiosBootHandle`].
    pub fn new(
        descriptor: PartitionDescriptor,
        read_fn: impl Fn() -> crate::Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            read_fn: Box::new(read_fn),
        }
    }

    /// Read the raw bytes of this partition.
    pub fn read_raw(&self) -> crate::Result<Vec<u8>> {
        (self.read_fn)()
    }
}
