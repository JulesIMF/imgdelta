# imgdelta

Delta compression tool for cloud OS images. Computes per-file binary deltas
between filesystem snapshots, stores patches in S3, and reconstructs images
offline. Built in Rust with xdelta3, pluggable encoders, and configurable
file-type routing.

## Status

> **Work in progress** — Phase 1 skeleton.

## Quick Start

```sh
# Install
cargo install --path image-delta-cli

# Compress a new image relative to a base image
imgdelta compress \
  --image /path/to/target.qcow2 \
  --base-image /path/to/base.qcow2 \
  --image-id debian-11-20260502 \
  --base-image-id debian-11-20260401 \
  --config imgdelta.toml

# Reconstruct the image
imgdelta decompress \
  --image-id debian-11-20260502 \
  --output /path/to/output/ \
  --config imgdelta.toml
```

## Documentation

- [User Guide](https://JulesIMF.github.io/imgdelta/) — architecture, configuration reference, integration guide
- [API Reference](https://JulesIMF.github.io/imgdelta/api/) — `cargo doc` output for `image-delta-core`

## Building

Requires:

- Rust stable (`rustup toolchain install stable`)
- C compiler (`gcc`) for vendored xdelta3

```sh
cargo build --all
cargo test --all
```

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

Third-party attributions: [NOTICE](NOTICE)
