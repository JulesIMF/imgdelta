# Contributing

## Dev tools setup

After cloning, install the git hooks and formatters:

```sh
# 1. Install tools
cargo install lefthook         # Git hook runner
cargo install taplo-cli        # TOML formatter
npm install --global prettier  # YAML + Markdown formatter (or use system package)

# 2. Activate hooks
lefthook install
```

The pre-commit hook will auto-format staged `*.rs`, `*.toml`, `*.yaml/yml`
and `*.md` files before every commit.

## Building

```sh
# Clone
git clone https://github.com/JulesIMF/imgdelta
cd imgdelta

# Build
cargo build --all

# Run tests
cargo test --all

# Check formatting
cargo fmt --all -- --check

# Lint
cargo clippy --all-targets -- -D warnings
```

## Code style

- `rustfmt` with default settings (enforced in CI)
- `clippy` with `-D warnings` (enforced in CI)
- All `unsafe` code confined to `image-delta-core/src/encoders/vcdiff/ffi.rs`

## Doc comments

Every public type and trait method must have a `///` doc comment.
Non-trivial functions should include a `# Examples` section with a compilable
example (these run as doc-tests in CI).

## Testing

- **L1 (unit + integration)**: `cargo test --all` — runs on any machine, no external deps
- **L2 (real images)**: `docs/yc-benchmark.sh` — requires a Yandex Cloud VM with attached disk

## Commit message format

```
{scope}: {message}
```

**Scope** follows the changed code path:

| Scope                | When to use                                                |
| -------------------- | ---------------------------------------------------------- |
| `core/manifest`      | changes to `image-delta-core/src/manifest.rs`              |
| `core/formats/qcow2` | changes specific to the qcow2 format                       |
| `core/formats`       | changes to the shared format traits                        |
| `cli/export`         | changes to a specific CLI subcommand                       |
| `git`                | changes to git hooks, lefthook config, `.gitmodules`, etc. |
| `all`                | wide-ranging changes that touch many modules               |

Rules:

- Use `cli/` prefix for `image-delta-cli`, `core/` for `image-delta-core`.
- Append sub-path components when the change is narrower (`core/encoders/vcdiff`).
- Keep the subject line ≤ 72 characters, imperative mood, no trailing period.
