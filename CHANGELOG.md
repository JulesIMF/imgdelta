# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- Cargo workspace with `image-delta-core` (library) and `image-delta-cli` (binary `imgdelta`)
- Trait definitions: `DeltaEncoder`, `Storage`, `Compressor`, `ImageFormat`, `RoutingRule`
- Encoder stubs: `Xdelta3Encoder`, `TextDiffEncoder`, `PassthroughEncoder`
- Built-in routing rules: `GlobRule`, `ElfRule`, `SizeRule`, `MagicRule`
- `DirectoryFormat` (no-op mount for plain directories)
- `RouterEncoder` with `select()` for per-file encoder dispatch
- CLI skeleton: `compress`, `decompress`, `image status/list`, `manifest inspect/diff`
- TOML config structure: `[storage]`, `[compressor]`, `[logging]`, `[[routing]]`
- GitHub Actions CI workflow
- mdBook structure for user guide

### Phase 2 — xdelta3 FFI + manifest serde

- Vendored `xdelta3.c` (v3.1.0) with `build.rs` linking it as a static library
- `encoders/vcdiff/ffi.rs` — only `unsafe` boundary in the codebase; thin wrapper around `xd3_encode_memory` / `xd3_decode_memory`
- `Xdelta3Encoder::encode` / `decode` — safe public API, no raw pointers exposed
- `PassthroughEncoder::encode` / `decode` — verbatim byte copy
- `TextDiffEncoder::encode` / `decode` — Myers line-level diff (pure Rust, `similar` crate)
- `ManifestHeader` + `Entry` + `EntryType` + `BlobRef` + `PatchRef` + `Metadata` structs
- MessagePack serialisation via `rmp-serde`; `None` fields skipped with `skip_serializing_if`
- `MANIFEST_VERSION = 1` constant

### Phase 3 — fs_diff, path_match, manifest rewrite

- `fs_diff::diff_dirs(base, target)` — `walkdir`-based tree comparison (symlinks not followed)
- `DiffKind`: `Added`, `Removed`, `Changed`, `MetadataOnly`
- `FileDiff { path, kind, base_meta, target_meta }` — per-file diff record
- `DiffResult { diffs, base: TreeStats, target: TreeStats }` — aggregate result
- `TreeStats { files, dirs, symlinks, total_bytes }` — tree metrics
- SHA-256 content hashing with mtime+size fast-path skip
- Mode, uid, gid, mtime comparison; symlink target comparison
- `tree_stats()` helper for computing `TreeStats` from a directory
- `path_match::find_best_matches(removed, added)` — bijective rename detection
- `PathMatchConfig { digit_weight }` — lower cost for digit substitutions (version bump tolerance)
- 12 unit tests in `fs_diff`

### Phase 4 — DefaultCompressor, FakeStorage, 85 tests, debug CLI

- `DefaultCompressor::compress(base_root, target_root, opts)` — full compress pipeline
- `DefaultCompressor::decompress(output_dir, opts)` — full decompress pipeline
- `build_inode_map` — hardlink detection by inode number
- Passthrough fallback when `delta.len() >= src.len() * passthrough_threshold`
- `FakeStorage` — in-memory `impl Storage` for L1 tests (no S3/DB)
- `compare_dirs` test helper — SHA-256, mode, uid, gid, mtime (±1 s), file type, symlinks, hardlinks, extended attributes
- xattr checking via `xattr::list()` + `xattr::get()` (`xattr = "1"` dependency)
- `UidMismatch`, `GidMismatch`, `XattrMismatch` variants in `DiffEntry` (test helper)
- `tests/fs_diff_integration.rs` — 10 bidirectional tests using `Scenario` builder
- `Scenario` builder: `write_base/target/both`, `chmod_*`, `symlink_*`, `age_base`, `hardlink_*`, `mkdir_*`
- `assert_symmetric` — asserts `diff_dirs(A,B)` and `diff_dirs(B,A)` are mirror images
- 8 compress/decompress round-trip integration tests
- `imgdelta debug walkdir OLD NEW` — debug-only subcommand (`#[cfg(debug_assertions)]`)
- `print_tree_stats()` with `fmt_bytes()` (B / KiB / MiB / GiB formatting)
- Total test count: **85 tests** (all passing, zero external deps)

### Fixed

- `vendor/xdelta3.h`: added explicit `#include <assert.h>` — Ubuntu 24.04 / GCC 13 no
  longer pulls it transitively through `<stdlib.h>`, causing `static_assert` to be
  undefined at file scope and breaking the CI build
- `image-delta-core/tests/fs_diff_integration.rs`: replaced 5× `&vec![…]` with slice
  literals `&[…]` to satisfy `clippy::useless_vec` (`-D warnings` in CI)
- `lefthook.yml`: added `clippy` command to `pre-commit` hook so lint errors are caught
  locally before reaching CI
