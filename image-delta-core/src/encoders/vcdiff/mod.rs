mod ffi;

use crate::{DeltaEncoder, Error, Result};

/// Delta encoder backed by xdelta3 (VCDIFF format).
///
/// Uses `xd3_encode_memory` / `xd3_decode_memory` via FFI for in-memory
/// VCDIFF encoding/decoding.  The xdelta3 C library is vendored at
/// `vendor/xdelta3.c` and compiled by `build.rs`.
///
/// # Safety
///
/// All unsafe code is isolated in `ffi.rs`.  This module only exposes a safe
/// Rust API.  Input pointers are valid for the duration of each FFI call, and
/// output buffers are allocated by Rust and checked for correctness before
/// returning to callers.
///
/// # Example
///
/// ```
/// use image_delta_core::{DeltaEncoder, Xdelta3Encoder};
///
/// let encoder = Xdelta3Encoder::new();
/// let source = b"hello world";
/// let target = b"hello rust!";
/// let delta = encoder.encode(source, target).unwrap();
/// let recovered = encoder.decode(source, &delta).unwrap();
/// assert_eq!(recovered, target);
/// ```
pub struct Xdelta3Encoder;

impl Xdelta3Encoder {
    pub fn new() -> Self {
        Self
    }

    /// Call `xd3_encode_memory` with automatic output-buffer growth.
    ///
    /// Starts with `initial_cap` bytes.  Doubles capacity and retries on
    /// `ENOSPC` up to 4 times.
    fn xd3_encode(source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
        // Worst case for VCDIFF: target size + a small header overhead.
        // We start generous and double on ENOSPC.
        let mut cap = (target.len() + 1024).max(1024);

        for _ in 0..5 {
            let mut buf = vec![0u8; cap];
            let mut out_size: ffi::UsiZeT = 0;

            let ret = unsafe {
                ffi::xd3_encode_memory(
                    target.as_ptr(),
                    target.len() as ffi::UsiZeT,
                    source.as_ptr(),
                    source.len() as ffi::UsiZeT,
                    buf.as_mut_ptr(),
                    &mut out_size,
                    cap as ffi::UsiZeT,
                    0, // flags
                )
            };

            if ret == 0 {
                buf.truncate(out_size as usize);
                return Ok(buf);
            }
            if ret == ffi::ENOSPC {
                cap *= 2;
                continue;
            }
            return Err(Error::Encode(format!("xd3_encode_memory errno={ret}")));
        }
        Err(Error::Encode(
            "xd3_encode_memory: output buffer exhausted after 5 retries".into(),
        ))
    }

    /// Call `xd3_decode_memory` with automatic output-buffer growth.
    fn xd3_decode(source: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
        // Decoded size is unknown; start with source.len() + delta.len() + slack.
        let mut cap = (source.len() + delta.len() + 1024).max(1024);

        for _ in 0..8 {
            let mut buf = vec![0u8; cap];
            let mut out_size: ffi::UsiZeT = 0;

            let ret = unsafe {
                ffi::xd3_decode_memory(
                    delta.as_ptr(),
                    delta.len() as ffi::UsiZeT,
                    source.as_ptr(),
                    source.len() as ffi::UsiZeT,
                    buf.as_mut_ptr(),
                    &mut out_size,
                    cap as ffi::UsiZeT,
                    0, // flags
                )
            };

            if ret == 0 {
                buf.truncate(out_size as usize);
                return Ok(buf);
            }
            if ret == ffi::ENOSPC {
                cap *= 2;
                continue;
            }
            return Err(Error::Decode(format!("xd3_decode_memory errno={ret}")));
        }
        Err(Error::Decode(
            "xd3_decode_memory: output buffer exhausted after 8 retries".into(),
        ))
    }
}

impl Default for Xdelta3Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl DeltaEncoder for Xdelta3Encoder {
    /// Encode `target` as a VCDIFF delta against `source`.
    fn encode(&self, source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
        Self::xd3_encode(source, target)
    }

    /// Decode a VCDIFF `delta` against `source`, returning the original target.
    fn decode(&self, source: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
        Self::xd3_decode(source, delta)
    }

    fn algorithm_id(&self) -> &'static str {
        "xdelta3"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoder() -> Xdelta3Encoder {
        Xdelta3Encoder::new()
    }

    // 1. Basic round-trip on small text content.
    #[test]
    fn encode_decode_roundtrip_small() {
        let enc = encoder();
        let source = b"hello world, this is the base file content";
        let target = b"hello rust!, this is the new file content!!";

        let delta = enc.encode(source, target).unwrap();
        let recovered = enc.decode(source, &delta).unwrap();

        assert_eq!(recovered, target);
    }

    // 2. Round-trip on larger binary content (512 KB).
    #[test]
    fn encode_decode_roundtrip_large() {
        let enc = encoder();

        // Build a pseudo-binary source: repeating pattern.
        let source: Vec<u8> = (0u8..=255).cycle().take(512 * 1024).collect();
        // Target: same pattern but with every 1000th byte flipped.
        let mut target = source.clone();
        for i in (0..target.len()).step_by(1000) {
            target[i] ^= 0xFF;
        }

        let delta = enc.encode(&source, &target).unwrap();
        let recovered = enc.decode(&source, &delta).unwrap();

        assert_eq!(recovered, target);
    }

    // 3. Delta for similar content is smaller than the target itself.
    #[test]
    fn delta_for_similar_content_is_compact() {
        let enc = encoder();
        let source: Vec<u8> = (0u8..=255).cycle().take(64 * 1024).collect();
        let mut target = source.clone();
        // Change 16 bytes in the middle.
        target[32768..32784].fill(0xAB);

        let delta = enc.encode(&source, &target).unwrap();

        // Delta must be much smaller than copying the whole target.
        assert!(
            delta.len() < target.len() / 4,
            "expected compact delta but got {} bytes (target {} bytes)",
            delta.len(),
            target.len()
        );
    }

    // 4. Encoding with empty source produces a valid (full-copy) delta.
    #[test]
    fn encode_empty_source_roundtrip() {
        let enc = encoder();
        let source: &[u8] = b"";
        let target = b"brand new file, no base";

        let delta = enc.encode(source, target).unwrap();
        let recovered = enc.decode(source, &delta).unwrap();

        assert_eq!(recovered, target);
    }

    // 5. Encoding identical source and target produces a valid (tiny) delta.
    #[test]
    fn encode_identical_content_roundtrip() {
        let enc = encoder();
        let content = b"identical content in both source and target";

        let delta = enc.encode(content, content).unwrap();
        let recovered = enc.decode(content, &delta).unwrap();

        assert_eq!(recovered.as_slice(), content);
    }

    // 6. Corrupted delta bytes must return an error, not panic.
    #[test]
    fn decode_corrupted_delta_returns_error() {
        let enc = encoder();
        let source = b"some base file content";
        let garbage_delta = b"this is not a valid vcdiff stream at all!!!!!";

        let result = enc.decode(source, garbage_delta);

        assert!(
            result.is_err(),
            "expected error for corrupted delta but got Ok"
        );
    }

    // 7. algorithm_id returns the expected string.
    #[test]
    fn algorithm_id_is_xdelta3() {
        assert_eq!(encoder().algorithm_id(), "xdelta3");
    }
}
