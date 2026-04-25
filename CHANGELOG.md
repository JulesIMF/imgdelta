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
