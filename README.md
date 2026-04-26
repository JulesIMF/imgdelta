# imgdelta

Delta compression tool for cloud OS images. Computes per-file binary deltas
between filesystem snapshots, stores patches in S3, and reconstructs images
offline. Built in Rust with vendored xdelta3, pluggable encoders, and
configurable file-type routing.

## Status

> **Phase 4 complete.** Compress/decompress pipeline implemented and covered
> by 85 L1 unit + integration tests. Phase 5 (real S3/PostgreSQL storage,
> Qcow2Image, parallel scheduler) is next.

## How it works

Traditional approach: store every image version in full → O(N) storage.

imgdelta approach:

```
base image ──┐
             ├──▶ diff_dirs() ──▶ path_match() ──▶ encode() ──▶ S3
new image  ──┘
```

1. **Walk** both filesystem trees, detect added/removed/changed/renamed files
2. **Route** each changed file to the right encoder by type (ELF→xdelta3,
   config→text-diff, already-compressed→passthrough, …)
3. **Encode** using VCDIFF (xdelta3 FFI) or text-diff or verbatim blob
4. **Upload** patches + manifest to S3; index metadata in PostgreSQL

Decompression downloads the manifest and patches, applies them in order,
and reconstructs the target filesystem offline (no daemon, no FUSE).

## Quick Start

### Prerequisites

- Rust stable (`rustup toolchain install stable`)
- C compiler (`gcc` or `clang`) — for the vendored `xdelta3.c`
- S3-compatible object storage and PostgreSQL (for Phase 5+)

### Install from source

```sh
git clone https://github.com/JulesIMF/imgdelta
cd imgdelta
cargo build --release --all
# binary is at ./target/release/imgdelta
```

### Minimal config (`imgdelta.toml`)

```toml
[storage]
s3_bucket    = "my-images"
s3_region    = "us-east-1"
database_url = "postgres://user:pass@localhost/imgdelta"

[compressor]
workers            = 8
default_encoder    = "xdelta3"
passthrough_threshold = 1.0   # never store a delta larger than the original

[[compressor.routing]]
type    = "glob"
pattern = "**/*.{gz,zst,xz,bz2,lz4}"
encoder = "passthrough"

[[compressor.routing]]
type    = "elf"
encoder = "xdelta3"
```

### Compress

```sh
imgdelta compress \
  --image    /mnt/new-image \
  --image-id debian-11-20260502 \
  --base-image-id debian-11-20260401 \
  --config   imgdelta.toml
```

### Decompress (reconstruct)

```sh
imgdelta decompress \
  --image-id   debian-11-20260502 \
  --base-root  /mnt/base-image \
  --output     /mnt/restored \
  --config     imgdelta.toml
```

### Inspect a manifest

```sh
imgdelta manifest inspect --image-id debian-11-20260502 --format json
```

## Crate structure

| Crate              | Role                                                             |
| ------------------ | ---------------------------------------------------------------- |
| `image-delta-core` | Library: all algorithms, traits, data structures. No S3/DB deps. |
| `image-delta-cli`  | Binary `imgdelta`: CLI, S3Storage impl, TOML config wiring.      |

Key modules inside `image-delta-core`:

| Module                 | Purpose                                                                       |
| ---------------------- | ----------------------------------------------------------------------------- |
| `fs_diff`              | Walk two directory trees → `DiffResult` (added/removed/changed/metadata-only) |
| `path_match`           | Bijective path-similarity scoring for rename detection                        |
| `encoders/vcdiff`      | xdelta3 FFI (unsafe confined here), `Xdelta3Encoder`                          |
| `encoders/passthrough` | Verbatim blob encoder for incompressible files                                |
| `routing`              | `RouterEncoder`, `GlobRule`, `ElfRule`, `SizeRule`, `MagicRule`               |
| `compressor`           | `DefaultCompressor`: orchestrates diff → match → encode → upload              |
| `manifest`             | MessagePack-serialised `Manifest` with per-file `Entry` records               |
| `storage`              | `Storage` trait (upload/download blobs, patches, manifests)                   |

## Testing

```sh
# All L1 tests (85 tests, no external deps)
cargo test --all

# A specific integration test file
cargo test --test compress_decompress

# Debug walkdir (debug builds only)
cargo build
./target/debug/imgdelta debug walkdir /path/to/base /path/to/new
```

Test infrastructure:

- `FakeStorage` — in-memory `Storage` for unit tests
- `compare_dirs` — deep tree comparison: SHA-256, mode, uid/gid, mtime, type,
  symlink targets, hardlinks, extended attributes (`xattr`)
- `fs_diff_integration` — 10 bidirectional scenarios with `assert_symmetric()`

## Documentation

- [User Guide](https://JulesIMF.github.io/imgdelta/) — quickstart, configuration reference, architecture, integration guide
- [API Reference](https://JulesIMF.github.io/imgdelta/api/) — `cargo doc` output for `image-delta-core`

Build docs locally:

```sh
cargo doc --no-deps -p image-delta-core --open
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

Third-party attributions: [NOTICE](NOTICE)
