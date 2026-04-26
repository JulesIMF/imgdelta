# Quick Start

## Prerequisites

| Requirement           | Version | Notes                                         |
| --------------------- | ------- | --------------------------------------------- |
| Rust                  | stable  | `rustup toolchain install stable`             |
| C compiler            | any     | `gcc` or `clang` — for vendored `xdelta3.c`   |
| S3-compatible storage | —       | AWS S3, Yandex Cloud Object Storage, or MinIO |
| PostgreSQL            | ≥ 14    | image metadata index                          |

For L1 tests only (no S3/DB): just Rust + C compiler.

## Install

```sh
git clone https://github.com/JulesIMF/imgdelta
cd imgdelta
cargo build --release --all
# add ./target/release to PATH or copy imgdelta to /usr/local/bin
```

## Minimal configuration

Create `imgdelta.toml`:

```toml
[storage]
s3_bucket    = "my-images"
s3_region    = "us-east-1"
database_url = "postgres://user:pass@localhost/imgdelta"

[compressor]
workers               = 8
default_encoder       = "xdelta3"
passthrough_threshold = 1.0

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

## Compress a new image

```sh
imgdelta compress \
  --image         /mnt/new-image \
  --image-id      debian-11-20260502 \
  --base-image-id debian-11-20260401 \
  --config        imgdelta.toml
```

On completion, imgdelta prints a summary:

```
Compressed debian-11-20260502 (base: debian-11-20260401)
  patched:  4 312 files  (87.3 MiB → 3.1 MiB, ratio 0.035)
  added:      127 files  (12.4 MiB verbatim)
  removed:     89 files
  elapsed:   14.2 s
```

## Decompress (reconstruct)

```sh
imgdelta decompress \
  --image-id  debian-11-20260502 \
  --base-root /mnt/base-image \
  --output    /mnt/restored \
  --config    imgdelta.toml
```

After decompression the directory `/mnt/restored` contains an exact
replica of the target filesystem with all file attributes (mode, uid/gid,
mtime, symlinks, hardlinks, xattrs) preserved.

## Inspect a manifest

```sh
# Pretty-print as JSON
imgdelta manifest inspect --image-id debian-11-20260502 --format json | jq .

# Check image status
imgdelta image status --image-id debian-11-20260502

# List all known images
imgdelta image list
```

## Debug (development builds)

Debug builds include the `debug` subcommand for inspecting real directory
pairs without running a full compress cycle:

```sh
cargo build   # debug profile
./target/debug/imgdelta debug walkdir /mnt/base /mnt/new
```

Example output:

```
~ usr/bin/grep
+ usr/lib/systemd/system/systemd-resolved.service
- usr/lib/systemd/system/ifupdown.service
M etc/sudoers

─── diff summary ────────────────────────
  +  added:          1
  -  removed:        1
  ~  changed:        1
  M  metadata-only:  1
  ─────────────────────────────────────────
     total diffs:    4

─── tree stats ──────────────────────────
  old (base)    45 231 files    2.1 GiB    3 812 dirs    1 024 symlinks
  new (target)  45 267 files    2.1 GiB    3 815 dirs    1 025 symlinks
```

## Run tests

```sh
# All 85 L1 tests (no external deps)
cargo test --all

# Specific integration file
cargo test --test compress_decompress -- --nocapture
```
