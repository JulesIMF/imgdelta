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

## License header

Every source file we own must carry a 5-line SPDX license header.
The header format differs by file type:

**Rust (`.rs`)**

```rust
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 <Author>
//
// image-delta — incremental disk-image compression toolkit
// <One-line description of this file's purpose>
```

**TOML / Shell / Env (`.toml`, `.sh`, `.env`, `.env.*`)**

```toml
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 <Author>
#
# image-delta — incremental disk-image compression toolkit
# <One-line description of this file's purpose>
```

For `.sh` files the header must come **after** the shebang line.

**SQL (`.sql`)**

```sql
-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 <Author>
--
-- image-delta — incremental disk-image compression toolkit
-- <One-line description of this file's purpose>
```

**Guidelines**

- `<Author>` — your name, nickname, or organisation (e.g. `JulesIMF`).
  Contributors retain copyright over their own contributions.
- `<year>` — the year the file was **created**; do not update it on every edit.
- `<description>` — a brief, imperative-style summary of the file's role, not a
  repetition of its name (e.g. `Eight-stage FS-partition compression pipeline`).
- The pre-commit hook (`scripts/check-license-header.sh`) will reject staged
  files that are missing the `SPDX-License-Identifier` line.
- To add headers to all files at once, run:

  ```sh
  python3 scripts/add-license-headers.py
  ```

  then review and adjust the auto-generated description lines.

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

**Scope** follows the changed code path.

### General scopes

These fixed keywords are always accepted by the commit-msg hook regardless of
the filesystem:

| Scope   | When to use                                                |
| ------- | ---------------------------------------------------------- |
| `all`   | wide-ranging changes that touch many modules               |
| `cargo` | changes to `Cargo.toml`, `Cargo.lock`, or workspace config |
| `ci`    | CI pipeline files (GitHub Actions, etc.)                   |
| `docs`  | documentation-only changes                                 |
| `git`   | git hooks, lefthook config, `.gitmodules`, etc.            |

### Path-based scopes

For everything else, the scope must correspond to a **real path** in the
repository. The commit-msg hook validates this automatically.

| Scope                | When to use                                   |
| -------------------- | --------------------------------------------- |
| `core/manifest`      | changes to `image-delta-core/src/manifest.rs` |
| `core/formats/qcow2` | changes specific to the qcow2 format          |
| `core/formats`       | changes to the shared format traits           |
| `cli/export`         | changes to a specific CLI subcommand          |

**How the hook resolves a path-based scope:**

1. `<scope>` → directory in repo root
2. `<scope>.rs` → Rust source file in repo root
3. For workspace crates (`core`, `cli`, …), `src/` is **injected automatically**:
   - `core/manifest` → `core/src/manifest.rs` ✓
   - `core/compress/stages` → `core/src/compress/stages/` ✓

> **Important:** Do **not** write `src/` explicitly in a scope — the hook will
> reject it with an error.\
> ✗ `core/src/manifest: …` → rejected\
> ✓ `core/manifest: …` → accepted

### Rules

- Scope characters: `[A-Za-z0-9_\-/]` — letters, digits, hyphens, underscores,
  slashes. No spaces, dots, or other punctuation.
- Use `cli/` prefix for `image-delta-cli`, `core/` for `image-delta-core`.
- Append sub-path components when the change is narrower (`core/encoders/vcdiff`).
- Keep the subject line ≤ 72 characters, imperative mood, no trailing period.
  _(these style rules are conventions; the hook does not enforce them)_

### Auto-skipped messages

The hook silently accepts the following without validation:

- Messages starting with `Merge `, `Revert `, `fixup! `, `squash! `, or
  `Initial commit`
- Empty messages and comment lines (starting with `#`)
