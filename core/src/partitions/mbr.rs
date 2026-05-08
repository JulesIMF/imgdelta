// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// MbrHandle — handle for the MBR boot-code area

use super::PartitionDescriptor;

/// Handle to the MBR boot-code area (bytes 0–439 of the raw disk).
///
/// This region precedes the partition table at offset 446 and contains
/// the GRUB stage-1 jump stub (or any other bootloader stage-1 code).
/// It is not a real partition-table entry; it is represented as a
/// synthetic partition with number 0 and kind [`PartitionKind::MbrBootCode`].
///
/// [`PartitionKind::MbrBootCode`]: crate::partitions::PartitionKind::MbrBootCode
pub struct MbrHandle {
    /// Synthetic descriptor (number = 0, kind = MbrBootCode).
    pub descriptor: PartitionDescriptor,
    read_fn: Box<dyn Fn() -> crate::Result<Vec<u8>> + Send + Sync>,
}

impl MbrHandle {
    /// Create a new [`MbrHandle`].
    pub fn new(
        descriptor: PartitionDescriptor,
        read_fn: impl Fn() -> crate::Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            read_fn: Box::new(read_fn),
        }
    }

    /// Read the 440 bytes of the MBR boot-code area.
    pub fn read_raw(&self) -> crate::Result<Vec<u8>> {
        (self.read_fn)()
    }
}
