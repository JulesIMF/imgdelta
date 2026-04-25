// All `unsafe` code in image-delta-core is confined to this file.
// The safe wrapper in `mod.rs` is the only public interface to the outside.
//
// xdelta3 single-file C library:
//   xd3_encode_memory(input, input_size, source, source_size,
//                     output_buf, output_size, avail_output, flags) -> int
//   xd3_decode_memory(input, input_size, source, source_size,
//                     output_buf, output_size, avail_output, flags) -> int
//
// `usize_t` in xdelta3.h resolves to `uint64_t` because XD3_USE_LARGESIZET=1
// is defined unconditionally in the header when targeting 64-bit platforms.
// We use `u64` for all `usize_t` parameters.
//
// Return value 0 = success; non-zero = errno (ENOSPC = 28 when the output
// buffer is too small).

use std::os::raw::c_int;

/// C `usize_t` as seen by xdelta3 on 64-bit targets.
pub type UsiZeT = u64;

/// Standard POSIX `ENOSPC` — returned by xdelta3 when the output buffer is
/// too small. Value 28 is stable on Linux/glibc and musl.
pub const ENOSPC: c_int = 28;

unsafe extern "C" {
    /// Encode `input` (new/target content) against `source` (base content)
    /// and write the VCDIFF delta to `output_buffer`.
    ///
    /// Returns 0 on success, `ENOSPC` when `avail_output` is insufficient,
    /// or another errno value on other errors.
    pub fn xd3_encode_memory(
        input: *const u8,
        input_size: UsiZeT,
        source: *const u8,
        source_size: UsiZeT,
        output_buffer: *mut u8,
        output_size: *mut UsiZeT,
        avail_output: UsiZeT,
        flags: c_int,
    ) -> c_int;

    /// Decode `input` (VCDIFF delta bytes) against `source` (base content)
    /// and write the reconstructed target to `output_buf`.
    ///
    /// Returns 0 on success, `ENOSPC` when `avail_output` is insufficient,
    /// or another errno value on other errors.
    pub fn xd3_decode_memory(
        input: *const u8,
        input_size: UsiZeT,
        source: *const u8,
        source_size: UsiZeT,
        output_buf: *mut u8,
        output_size: *mut UsiZeT,
        avail_output: UsiZeT,
        flags: c_int,
    ) -> c_int;
}
