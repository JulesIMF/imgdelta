# Configuration Reference

imgdelta is configured via a TOML file (passed with `--config imgdelta.toml`).
All fields have documented defaults.

## `[storage]`

S3 + PostgreSQL backend.

```toml
[storage]
s3_bucket    = "my-images"          # required
s3_region    = "us-east-1"          # optional, defaults to AWS_REGION env var
s3_endpoint  = "https://s3.yandexcloud.net"  # optional, override for non-AWS
database_url = "postgres://user:pass@localhost/imgdelta"  # required
```

| Field          | Type   | Default          | Description                   |
| -------------- | ------ | ---------------- | ----------------------------- |
| `s3_bucket`    | string | —                | S3 bucket name                |
| `s3_region`    | string | env `AWS_REGION` | AWS region                    |
| `s3_endpoint`  | string | AWS default      | Override endpoint (MinIO, YC) |
| `database_url` | string | —                | PostgreSQL connection string  |

## `[compressor]`

Controls parallelism, fallback behaviour, default encoder, and routing rules.

```toml
[compressor]
workers               = 8       # default: number of logical CPUs
passthrough_threshold = 1.0     # store verbatim if delta >= original * threshold
default_encoder       = "xdelta3"
```

| Field                   | Type    | Default     | Description                           |
| ----------------------- | ------- | ----------- | ------------------------------------- |
| `workers`               | integer | `num_cpus`  | Parallel worker threads               |
| `passthrough_threshold` | float   | `1.0`       | Delta/original size ratio cutoff      |
| `default_encoder`       | enum    | `"xdelta3"` | Fallback when no routing rule matches |

### Encoder values

| Value           | Algorithm                              |
| --------------- | -------------------------------------- |
| `"xdelta3"`     | VCDIFF binary delta (vendored xdelta3) |
| `"text_diff"`   | Myers line-level diff (pure Rust)      |
| `"passthrough"` | Verbatim blob (no delta)               |

## `[[compressor.routing]]`

An ordered list of rules. The first rule whose predicate matches a file wins.
Files that match no rule use `default_encoder`.

### `type = "glob"`

```toml
[[compressor.routing]]
type    = "glob"
pattern = "**/*.{gz,zst,xz,bz2,lz4,br,zip}"
encoder = "passthrough"
```

| Field     | Description                                         |
| --------- | --------------------------------------------------- |
| `pattern` | Glob pattern matched against the relative file path |
| `encoder` | Encoder to use for matching files                   |

### `type = "elf"`

Matches files whose first 4 bytes are `\x7fELF` (Linux ELF binaries and shared libraries).

```toml
[[compressor.routing]]
type    = "elf"
encoder = "xdelta3"
```

### `type = "size"`

Matches files smaller than `max_bytes`.

```toml
[[compressor.routing]]
type      = "size"
max_bytes = 512
encoder   = "passthrough"
```

### `type = "magic"`

Matches files whose bytes at `offset` start with the given hex sequence.

```toml
[[compressor.routing]]
type    = "magic"
offset  = 0
hex     = "89504e47"   # PNG magic bytes
encoder = "passthrough"
```

| Field    | Description                               |
| -------- | ----------------------------------------- |
| `offset` | Byte offset from file start (default `0`) |
| `hex`    | Hex-encoded byte sequence to match        |

## `[logging]`

```toml
[logging]
level = "info"   # error | warn | info | debug | trace
file  = "/var/log/imgdelta/compress.log"  # optional

[logging.targets]
"image_delta_core::fs_diff"    = "debug"
"image_delta_core::path_match" = "debug"
"image_delta_core::scheduler"  = "warn"
```

| Field     | Default  | Description                                  |
| --------- | -------- | -------------------------------------------- |
| `level`   | `"info"` | Global log level                             |
| `file`    | —        | Write logs to file in addition to stderr     |
| `targets` | `{}`     | Per-module overrides (tracing target syntax) |

`RUST_LOG` environment variable overrides `[logging]` when set.

## Full example

```toml
[storage]
s3_bucket    = "acme-images"
s3_region    = "eu-central-1"
database_url = "postgres://imgdelta:secret@db.internal/imgdelta"

[compressor]
workers               = 16
passthrough_threshold = 1.0
default_encoder       = "xdelta3"

# Already-compressed formats → no delta
[[compressor.routing]]
type    = "glob"
pattern = "**/*.{gz,zst,xz,bz2,lz4,br,zip,png,jpg,mp4,webm}"
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

[logging]
level = "info"
file  = "/var/log/imgdelta/last.log"

[logging.targets]
"image_delta_core::compressor" = "debug"
```
