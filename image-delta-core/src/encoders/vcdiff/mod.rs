mod ffi;

use crate::{DeltaEncoder, Result};

/// Delta encoder backed by xdelta3 (VCDIFF format).
///
/// Uses `xd3_encode_memory` / `xd3_decode_memory` via FFI for in-memory
/// operation on files ≤ 10 MB, and a streaming API for larger files.
///
/// The xdelta3 C library is vendored at `vendor/xdelta3.c` and compiled by
/// `build.rs`.
pub struct Xdelta3Encoder;

impl Xdelta3Encoder {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Xdelta3Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl DeltaEncoder for Xdelta3Encoder {
    fn encode(&self, _source: &[u8], _target: &[u8]) -> Result<Vec<u8>> {
        todo!("Phase 2: xdelta3 FFI — xd3_encode_memory")
    }

    fn decode(&self, _source: &[u8], _delta: &[u8]) -> Result<Vec<u8>> {
        todo!("Phase 2: xdelta3 FFI — xd3_decode_memory")
    }

    fn algorithm_id(&self) -> &'static str {
        "xdelta3"
    }
}
