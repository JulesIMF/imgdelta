# Quick Start

## Prerequisites

| Requirement  | Version | Notes                                           |
| ------------ | ------- | ----------------------------------------------- |
| Rust         | stable  | `rustup toolchain install stable`               |
| C compiler   | any     | `gcc` or `clang` — for vendored `xdelta3.c`     |
| `qemu-utils` | —       | Required **only** for qcow2 images (`qemu-nbd`) |

For directory-format images (or L1 unit tests): only Rust + C compiler needed.

## Build from source

```sh
git clone https://github.com/JulesIMF/imgdelta
cd imgdelta
cargo build --release --all
# binary: ./target/release/imgdelta
```

## Minimal configuration

Copy `examples/imgdelta.toml` and adjust:

```toml
[storage]
type      = "local"
local_dir = "/var/lib/imgdelta"   # any writable directory

[compressor]
workers         = 8
default_encoder = "xdelta3"

# Already-compressed formats → store verbatim
[[compressor.routing]]
type    = "glob"
pattern = "**/*.{gz,zst,xz,bz2,lz4,br}"
encoder = "passthrough"

# ELF binaries → xdelta3 VCDIFF
[[compressor.routing]]
type    = "elf"
encoder = "xdelta3"
```

> The `local` storage backend stores all data under `local_dir` as ordinary
> files — no S3, no PostgreSQL. It is the built-in reference implementation
> and suitable for single-machine use and development. Providers running at
> scale should implement a custom `Storage` backed by their own object store.

## Compress an image

```sh
imgdelta compress \
  --image         /mnt/new-image \
  --image-id      debian-11-20260502 \
  --base-image-id debian-11-20260401 \
  --config        imgdelta.toml
```

On completion imgdelta prints a summary:

```
Compressed debian-11-20260502 (base: debian-11-20260401)
  patched:    4 312 files  (87.3 MiB → 3.1 MiB, ratio 0.035)
  added:        127 files  (12.4 MiB verbatim)
  removed:       89 files
  renamed:       14 files
  elapsed:     14.2 s
```

## Decompress (reconstruct)

```sh
imgdelta decompress \
  --image-id  debian-11-20260502 \
  --base-root /mnt/base-image \
  --output    /mnt/restored \
  --config    imgdelta.toml
```

After decompression `/mnt/restored` contains an exact replica of the target
filesystem with all attributes preserved (mode, uid/gid, mtime, symlinks,
hardlinks, xattrs).

## Compress a qcow2 image

```sh
imgdelta compress \
  --image-format  qcow2 \
  --image         /images/fedora-37-new.qcow2 \
  --image-id      fedora-37-20260502 \
  --base-image-id fedora-37-20260401 \
  --config        imgdelta.toml
```

Requires `qemu-nbd` and `CAP_SYS_ADMIN` (or run as root).

## Inspect a manifest

```sh
# Pretty-print as JSON
imgdelta manifest inspect --image-id debian-11-20260502 --format json | jq .

# Check image lifecycle status
imgdelta image status --image-id debian-11-20260502

# List all known images
imgdelta image list
```

## Debug walkdir (development builds only)

Inspect a directory pair without running a full compress cycle:

```sh
cargo build   # debug profile
./target/debug/imgdelta debug walkdir /mnt/base /mnt/new
```

Output:

```
+ usr/bin/new-tool
- usr/bin/old-tool
~ etc/resolv.conf
M etc/hostname

─── diff summary ────────────────────────
  +  added:         1
  -  removed:       1
  ~  changed:       1
  M  metadata-only: 1
     total diffs:   4

─── tree stats ──────────────────────────
  old (base)    45 231 files    2.1 GiB    3 812 dirs    1 024 symlinks
  new (target)  45 267 files    2.1 GiB    3 815 dirs    1 025 symlinks
```

## Run the test suite

```sh
# All tests (no external dependencies required)
cargo test --all

# Specific integration file
cargo test --test compress_decompress -- --nocapture

# Roundtrip test on real qcow2 images (requires mounted images)
scripts/roundtrip-test.sh
```
