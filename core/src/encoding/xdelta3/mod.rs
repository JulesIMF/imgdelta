// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Xdelta3Encoder: VCDIFF (RFC 3284) delta encoder backed by libxdelta3

mod ffi;

use super::PatchAlgorithm;
use super::{AlgorithmCode, FilePatch, FileSnapshot, PatchEncoder};
use crate::{Error, Result};

// ── Xdelta3Algorithm ──────────────────────────────────────────────────────────

/// Raw VCDIFF algorithm implementation via xdelta3 FFI.
///
/// This is the low-level worker. [`Xdelta3Encoder`] instantiates it per call.
/// Zero-size type — instantiation is free.
pub(crate) struct Xdelta3Algorithm;

impl Xdelta3Algorithm {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl PatchAlgorithm for Xdelta3Algorithm {
    fn encode_raw(&self, source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
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

    fn decode_raw(&self, source: &[u8], patch: &[u8]) -> Result<Vec<u8>> {
        // Decoded size is unknown; start with source.len() + patch.len() + slack.
        let mut cap = (source.len() + patch.len() + 1024).max(1024);

        for _ in 0..8 {
            let mut buf = vec![0u8; cap];
            let mut out_size: ffi::UsiZeT = 0;

            let ret = unsafe {
                ffi::xd3_decode_memory(
                    patch.as_ptr(),
                    patch.len() as ffi::UsiZeT,
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

// ── Xdelta3Encoder ────────────────────────────────────────────────────────────

/// Patch encoder backed by xdelta3 (VCDIFF format).
///
/// Uses `xd3_encode_memory` / `xd3_decode_memory` via FFI for in-memory
/// VCDIFF encoding/decoding.  The xdelta3 C library is vendored at
/// `vendor/xdelta3.c` and compiled by `build.rs`.
///
/// Internally creates a [`Xdelta3Algorithm`] per encode/decode call.
/// That type is zero-size, so instantiation is free.
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
/// use image_delta_core::{FileSnapshot, PatchEncoder, Xdelta3Encoder};
///
/// let encoder = Xdelta3Encoder::new();
/// let source = b"hello world";
/// let target = b"hello rust!";
/// let base = FileSnapshot { bytes: source, path: "lib/hello.so", size: source.len() as u64, header: source };
/// let tgt  = FileSnapshot { bytes: target, path: "lib/hello.so", size: target.len() as u64, header: target };
/// let patch = encoder.encode(&base, &tgt).unwrap();
/// let recovered = encoder.decode(source, &patch).unwrap();
/// assert_eq!(recovered, target);
/// ```
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

impl PatchEncoder for Xdelta3Encoder {
    /// Encode `target.bytes` as a VCDIFF patch against `base.bytes`.
    fn encode(&self, base: &FileSnapshot<'_>, target: &FileSnapshot<'_>) -> Result<FilePatch> {
        let bytes = Xdelta3Algorithm::new().encode_raw(base.bytes, target.bytes)?;
        Ok(FilePatch::new(bytes, AlgorithmCode::Xdelta3))
    }

    /// Decode a VCDIFF patch against `source`, returning the original target.
    fn decode(&self, source: &[u8], patch: &FilePatch) -> Result<Vec<u8>> {
        Xdelta3Algorithm::new().decode_raw(source, &patch.bytes)
    }

    fn algorithm_code(&self) -> Option<AlgorithmCode> {
        Some(AlgorithmCode::Xdelta3)
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

    fn snap<'a>(bytes: &'a [u8]) -> FileSnapshot<'a> {
        FileSnapshot {
            bytes,
            path: "",
            size: bytes.len() as u64,
            header: &bytes[..bytes.len().min(16)],
        }
    }

    // 1. Basic round-trip on small text content.
    #[test]
    fn encode_decode_roundtrip_small() {
        let enc = encoder();
        let source = b"hello world, this is the base file content";
        let target = b"hello rust!, this is the new file content!!";

        let patch = enc.encode(&snap(source), &snap(target)).unwrap();
        let recovered = enc.decode(source, &patch).unwrap();

        assert_eq!(recovered, target);
    }

    // 2. Round-trip on larger binary content (512 KB).
    #[test]
    fn encode_decode_roundtrip_large() {
        let enc = encoder();

        let source: Vec<u8> = (0u8..=255).cycle().take(512 * 1024).collect();
        let mut target = source.clone();
        for i in (0..target.len()).step_by(1000) {
            target[i] ^= 0xFF;
        }

        let patch = enc.encode(&snap(&source), &snap(&target)).unwrap();
        let recovered = enc.decode(&source, &patch).unwrap();

        assert_eq!(recovered, target);
    }

    // 3. Patch for similar content is smaller than the target itself.
    #[test]
    fn patch_for_similar_content_is_compact() {
        let enc = encoder();
        let source: Vec<u8> = (0u8..=255).cycle().take(64 * 1024).collect();
        let mut target = source.clone();
        target[32768..32784].fill(0xAB);

        let patch = enc.encode(&snap(&source), &snap(&target)).unwrap();

        assert!(
            patch.bytes.len() < target.len() / 4,
            "expected compact patch but got {} bytes (target {} bytes)",
            patch.bytes.len(),
            target.len()
        );
    }

    // 4. Encoding with empty source produces a valid (full-copy) patch.
    #[test]
    fn encode_empty_source_roundtrip() {
        let enc = encoder();
        let source: &[u8] = b"";
        let target = b"brand new file, no base";

        let patch = enc.encode(&snap(source), &snap(target)).unwrap();
        let recovered = enc.decode(source, &patch).unwrap();

        assert_eq!(recovered, target);
    }

    // 5. Encoding identical source and target produces a valid (tiny) patch.
    #[test]
    fn encode_identical_content_roundtrip() {
        let enc = encoder();
        let content = b"identical content in both source and target";

        let patch = enc.encode(&snap(content), &snap(content)).unwrap();
        let recovered = enc.decode(content, &patch).unwrap();

        assert_eq!(recovered.as_slice(), content);
    }

    // 6. Corrupted patch bytes must return an error, not panic.
    #[test]
    fn decode_corrupted_patch_returns_error() {
        let enc = encoder();
        let source = b"some base file content";
        let garbage_bytes = b"this is not a valid vcdiff stream at all!!!!!";
        let garbage_patch = FilePatch::new(garbage_bytes.to_vec(), AlgorithmCode::Xdelta3);

        let result = enc.decode(source, &garbage_patch);

        assert!(
            result.is_err(),
            "expected error for corrupted patch but got Ok"
        );
    }

    // 7. algorithm_id returns the expected string.
    #[test]
    fn algorithm_id_is_xdelta3() {
        assert_eq!(encoder().algorithm_id(), "xdelta3");
    }

    // 8. algorithm_code returns Xdelta3.
    #[test]
    fn algorithm_code_is_xdelta3() {
        assert_eq!(encoder().algorithm_code(), Some(AlgorithmCode::Xdelta3));
    }

    // 9. FilePatch carries correct code.
    #[test]
    fn encode_result_carries_xdelta3_code() {
        let enc = encoder();
        let patch = enc
            .encode(&snap(b"old"), &snap(b"new content here"))
            .unwrap();
        assert_eq!(patch.code, AlgorithmCode::Xdelta3);
        assert_eq!(patch.algorithm_id, None);
    }
}
