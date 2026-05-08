// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// RawHandle — handle for raw (unformatted) partitions

use super::PartitionDescriptor;

/// Handle to a raw (unformatted) partition.
pub struct RawHandle {
    /// Partition descriptor from the image's partition table.
    pub descriptor: PartitionDescriptor,
    read_fn: Box<dyn Fn() -> crate::Result<Vec<u8>> + Send + Sync>,
}

impl RawHandle {
    /// Create a new [`RawHandle`].
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
