# imgdelta

Delta compression toolkit for cloud OS images. imgdelta computes per-file
binary deltas between consecutive filesystem snapshots, stores compact patch
archives in any pluggable storage backend, and reconstructs images offline —
**without any daemon, FUSE mount, or cloud SDK dependency in the core library**.

Built as a diploma project with industrial applicability in mind.

## How it works

Traditional approach: store every image version in full → O(N) storage cost.

imgdelta approach:

```
base image ──┐
             ├──▶ walkdir ──▶ rename-detect ──▶ route ──▶ encode ──▶ Storage
new image  ──┘
```

1. **Walk** both filesystem trees; detect added/removed/changed/renamed files
2. **Route** each changed file to the right encoder by type
   (ELF → xdelta3, config → text-diff, already-compressed → passthrough, …)
3. **Encode** using VCDIFF (xdelta3 FFI), Myers text-diff, or verbatim blob
4. **Upload** patches + manifest (MessagePack) to storage

Decompression downloads the manifest and patches, applies them in order, and
reconstructs the target filesystem exactly.

Benchmarked on 551 real cloud image pairs across Ubuntu 22.04, Debian 11,
Fedora 37, and CentOS Stream 8 — imgdelta achieves **42–114× compression
ratio** vs full rootfs size (vs 2–5× for qcow2 backing chains).

## Quick start

### Prerequisites

- Rust stable (`rustup toolchain install stable`)
- C compiler (`gcc` or `clang`) — for the vendored `xdelta3.c`
- For qcow2 images: `qemu-nbd` and `CAP_SYS_ADMIN`

### Build from source

```sh
git clone https://github.com/JulesIMF/imgdelta
cd imgdelta
cargo build --release
# binary: ./target/release/imgdelta
```

### Minimal config (`imgdelta.toml`)

```toml
[storage]
type      = "local"
local_dir = "/srv/imgdelta"

[compressor]
workers = 8

[[compressor.routing]]
type    = "glob"
pattern = "**/*.{gz,zst,xz,bz2}"
encoder = "passthrough"

[[compressor.routing]]
type    = "elf"
encoder = "xdelta3"

# fallback
[[compressor.routing]]
type    = "glob"
pattern = "**/*"
encoder = "xdelta3"
```

A full annotated example is in [`examples/imgdelta.toml`](examples/imgdelta.toml).

### Compress

```sh
imgdelta --config imgdelta.toml compress \
  --image-id      debian-11-20260502 \
  --base-image-id debian-11-20260401 \
  --source        /mnt/base-image \
  --target        /mnt/new-image
```

### Decompress

```sh
imgdelta --config imgdelta.toml decompress \
  --image-id  debian-11-20260502 \
  --base-root /mnt/base-image \
  --output    /mnt/restored
```

### Inspect a manifest

```sh
imgdelta --config imgdelta.toml manifest inspect --image-id debian-11-20260502
```

## Crate structure

| Crate                      | Role                                                                                                       |
| -------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `image-delta-core`         | Library: all algorithms, traits, data structures, `LocalStorage` reference impl. Zero cloud SDK / DB deps. |
| `image-delta-cli`          | Binary `imgdelta`: CLI, TOML config wiring.                                                                |
| `image-delta-synthetic-fs` | Test helper: deterministic filesystem tree generator and mutator.                                          |

## Testing

```sh
# Unit + integration tests (no external deps)
cargo test --all

# Real qcow2 roundtrip (requires qemu-nbd + CAP_SYS_ADMIN)
bash scripts/roundtrip-test.sh
```

Test infrastructure:

- `FakeStorage` — in-memory `Storage` mock; no I/O
- `image-delta-synthetic-fs` — generates realistic base/target directory pairs
  (`FsTreeBuilder` + `FsMutator`)
- `compare_dirs` — deep tree comparison: SHA-256, mode, uid/gid, mtime,
  symlink targets, hardlinks, xattrs

## Documentation

- **[User Guide](https://JulesIMF.github.io/imgdelta/)** — quickstart,
  configuration reference, architecture, provider integration guide
- **[API Reference](https://JulesIMF.github.io/imgdelta/api/)** — `cargo doc`
  output for `image-delta-core`

Build docs locally:

```sh
cargo install mdbook mdbook-mermaid
mdbook serve book --open
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

Third-party attributions: [NOTICE](NOTICE)
