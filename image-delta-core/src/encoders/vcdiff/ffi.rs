// Phase 2: unsafe xdelta3 C FFI bindings.
//
// All `unsafe` code in image-delta-core is confined to this file.
// The safe wrapper in `mod.rs` is the only public interface.
//
// Functions exposed by xdelta3 single-file C library (vendor/xdelta3.c):
//   xd3_encode_memory  — in-memory encode (source + target → delta)
//   xd3_decode_memory  — in-memory decode (source + delta → target)
//
// For files > 10 MB a streaming API is used instead (Phase 2).
