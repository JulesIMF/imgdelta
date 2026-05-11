# Configuration Reference

imgdelta is configured via a TOML file, passed with `--config imgdelta.toml`.

## `[storage]`

Storage backend selection. Currently one built-in type is supported; cloud
providers implement the `Storage` trait directly in their own Rust code.

### `type = "local"` (built-in reference backend)

```toml
[storage]
type      = "local"
local_dir = "/var/lib/imgdelta"
```

All blobs, manifests, and patch archives are stored as regular files under
`local_dir`:

```
{local_dir}/
  blobs/{uuid}                   — blob bytes (gzip-compressed when smaller)
  images/{image_id}/manifest     — MessagePack manifest
  images/{image_id}/patches.tar  — patches tar archive
  images/{image_id}/meta.json    — ImageMeta + status
  sha256_index.json              — sha256 hex → {uuid, compressed} (dedup)
  blobs.json                     — blob origin records
```

Blob UUIDs are deterministic (UUID v5 over SHA-256) so re-uploading the same
content is idempotent.

> **For providers**: do not use `LocalStorage` in production at scale.
> Implement `Storage` for your own object store and metadata service.
> See [Provider Integration](integration.md).

## `[compressor]`

```toml
[compressor]
workers               = 8       # default: logical CPU count
passthrough_threshold = 1.0     # store verbatim if delta_size >= source_size * threshold
default_encoder       = "xdelta3"
```

| Field                   | Type    | Default     | Description                          |
| ----------------------- | ------- | ----------- | ------------------------------------ |
| `workers`               | integer | `num_cpus`  | Parallel rayon worker threads        |
| `passthrough_threshold` | float   | `1.0`       | Delta/original size ratio cutoff     |
| `default_encoder`       | enum    | `"xdelta3"` | Encoder when no routing rule matches |

### Encoder values

| Value           | Algorithm                                        |
| --------------- | ------------------------------------------------ |
| `"xdelta3"`     | VCDIFF binary delta (vendored xdelta3 C library) |
| `"text_diff"`   | Myers line-level diff (pure Rust, `diffy` crate) |
| `"passthrough"` | Verbatim blob (no delta, stores the file as-is)  |

## `[[compressor.routing]]`

An ordered list of routing rules. The first rule whose predicate matches a
file wins. Files matching no rule use `default_encoder`.

### `type = "glob"`

```toml
[[compressor.routing]]
type    = "glob"
pattern = "**/*.{gz,zst,xz,bz2,lz4,br}"
encoder = "passthrough"
```

`pattern` is a glob matched against the relative file path (forward-slash
separated).

### `type = "elf"`

Matches files whose first 4 bytes are `\x7fELF`.

```toml
[[compressor.routing]]
type    = "elf"
encoder = "xdelta3"
```

### `type = "size"`

Matches files strictly smaller than `max_bytes`.

```toml
[[compressor.routing]]
type      = "size"
max_bytes = 512
encoder   = "passthrough"
```

### `type = "magic"`

Matches files whose bytes at `offset` (default `0`) start with the given
hex sequence.

```toml
[[compressor.routing]]
type   = "magic"
hex    = "89504e47"    # PNG magic bytes
encoder = "passthrough"
```

| Field    | Description                               |
| -------- | ----------------------------------------- |
| `hex`    | Hex-encoded byte sequence to match        |
| `offset` | Byte offset from file start (default `0`) |

## `[logging]`

```toml
[logging]
level = "info"              # error | warn | info | debug | trace
file  = "/var/log/imgdelta/compress.log"   # optional

[logging.targets]
"image_delta_core::fs_diff"    = "debug"
"image_delta_core::path_match" = "debug"
```

`RUST_LOG` overrides `[logging]` when set.

## Full example

```toml
[storage]
type      = "local"
local_dir = "/var/lib/imgdelta"

[compressor]
workers               = 16
passthrough_threshold = 1.0
default_encoder       = "xdelta3"

# Already-compressed and media formats → no delta
[[compressor.routing]]
type    = "glob"
pattern = "**/*.{gz,zst,xz,bz2,lz4,br,zip,png,jpg,jpeg,mp4,webm}"
encoder = "passthrough"

# ELF binaries → xdelta3
[[compressor.routing]]
type    = "elf"
encoder = "xdelta3"

# Config and script files → line-level diff
[[compressor.routing]]
type    = "glob"
pattern = "**/*.{conf,cfg,ini,yaml,toml,json,py,sh,rb,pl}"
encoder = "text_diff"

# Tiny files → not worth the delta overhead
[[compressor.routing]]
type      = "size"
max_bytes = 512
encoder   = "passthrough"

[logging]
level = "info"

[logging.targets]
"image_delta_core::compress" = "debug"
```
