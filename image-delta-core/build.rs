fn main() {
    // Detect pointer size and unsigned-long-long size for xdelta3 configure checks.
    // xdelta3.h contains static_assert(SIZEOF_SIZE_T == sizeof(size_t)) etc.
    let sizeof_size_t = std::mem::size_of::<usize>().to_string();
    let sizeof_ull = std::mem::size_of::<std::os::raw::c_ulonglong>().to_string();

    cc::Build::new()
        .file("vendor/xdelta3.c")
        // Include our vendor directory so all xdelta3-*.h headers are found.
        .include("vendor")
        // xdelta3 configure-generated defines (we replace config.h with explicit defines).
        .define("XD3_ENCODER", "1")
        .define("HAVE_CONFIG_H", "0")
        .define("SIZEOF_SIZE_T", sizeof_size_t.as_str())
        .define("SIZEOF_UNSIGNED_LONG_LONG", sizeof_ull.as_str())
        // Disable secondary compression (lzma/djw) — not needed for VCDIFF only.
        .define("SECONDARY_DJW", "0")
        .define("SECONDARY_FGK", "0")
        .define("SECONDARY_LZMA", "0")
        // Silence upstream warnings — we do not own this code.
        .warnings(false)
        .compile("xdelta3");

    // Re-run if the vendored source changes.
    println!("cargo:rerun-if-changed=vendor/xdelta3.c");
    println!("cargo:rerun-if-changed=vendor/xdelta3.h");
}
