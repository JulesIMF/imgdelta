#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 JulesIMF
#
# image-delta — incremental disk-image compression toolkit
# commit-msg hook: validate commit message header format

"""
Validates that the first line of the commit message looks like:

    {module}: {description}

where {module} is one of:
  - a hardcoded general module (all, ci, git, cargo, docs)
  - an existing directory path relative to the repo root
  - a path to a Rust file (without .rs extension)

For workspace crates discovered in Cargo.toml, the convention allows
omitting the intermediate "src/" directory in the module path:
  core/compress/stages  →  core/src/compress/stages   (directory)
  cli/commands/image    →  cli/src/commands/image.rs   (file)
"""

import os
import re
import sys

# ── Hardcoded general modules ─────────────────────────────────────────────────

GENERAL_MODULES = {"all", "ci", "git", "cargo", "docs", "scripts"}

# ── Helpers ───────────────────────────────────────────────────────────────────


def parse_cargo_members(cargo_toml_path: str) -> list[str]:
    """Return the list of workspace member directory names from Cargo.toml.

    Uses only the standard library — no third-party TOML parser.
    Parses the ``members = [...]`` array inside the ``[workspace]`` section.
    """
    try:
        with open(cargo_toml_path, encoding="utf-8") as fh:
            content = fh.read()
    except OSError:
        return []

    workspace_match = re.search(r"\[workspace\]", content)
    if not workspace_match:
        return []

    after_workspace = content[workspace_match.end():]

    # Stop at the next section header so we don't bleed into other tables.
    next_section = re.search(r"^\[", after_workspace, re.MULTILINE)
    if next_section:
        after_workspace = after_workspace[: next_section.start()]

    members_match = re.search(
        r"members\s*=\s*\[([^\]]*)\]", after_workspace, re.DOTALL
    )
    if not members_match:
        return []

    return re.findall(r'"([^"]+)"', members_match.group(1))


def find_git_root() -> str | None:
    """Walk up from cwd until a .git entry (file or directory) is found.

    A `.git` *file* indicates a git submodule or worktree — we still treat
    the containing directory as the repository root for our purposes.
    """
    path = os.path.abspath(".")
    while True:
        git = os.path.join(path, ".git")
        if os.path.isdir(git) or os.path.isfile(git):
            return path
        parent = os.path.dirname(path)
        if parent == path:
            return None
        path = parent


def module_path_exists(module: str, repo_root: str, crates: list[str]) -> bool:
    """Return True if *module* maps to a real path in the repository.

    Checks in order:
    1. ``<repo_root>/<module>``          — directory
    2. ``<repo_root>/<module>.rs``       — Rust source file
    3. ``<repo_root>/<crate>/src/<rest>`` — directory  (src-injection)
    4. ``<repo_root>/<crate>/src/<rest>.rs`` — file    (src-injection)

    Steps 3-4 are tried for every crate whose name is a prefix of *module*,
    but only when the path after the crate prefix does **not** already start
    with ``src/``.
    """
    base = repo_root

    if os.path.isdir(os.path.join(base, module)):
        return True
    if os.path.isfile(os.path.join(base, module + ".rs")):
        return True

    for crate in crates:
        prefix = crate + "/"
        if not module.startswith(prefix):
            continue
        rest = module[len(prefix):]
        # Don't double-insert if the caller already wrote "src/".
        if rest.startswith("src/") or rest == "src":
            continue
        expanded = os.path.join(crate, "src", rest)
        if os.path.isdir(os.path.join(base, expanded)):
            return True
        if os.path.isfile(os.path.join(base, expanded + ".rs")):
            return True

    return False


# ── Main ──────────────────────────────────────────────────────────────────────


def main() -> None:
    if len(sys.argv) < 2:
        print("Usage: check-commit-msg.py <commit-message-file>", file=sys.stderr)
        sys.exit(1)

    with open(sys.argv[1], encoding="utf-8") as fh:
        lines = fh.readlines()

    header = lines[0].rstrip("\n") if lines else ""

    # Skip auto-generated or special commit messages.
    if re.match(r"^(Merge |Revert |fixup! |squash! |Initial commit)", header):
        sys.exit(0)
    # Skip empty (e.g. --allow-empty-message)
    if not header or header.startswith("#"):
        sys.exit(0)

    # Validate format: "{module}: {description}"
    match = re.fullmatch(r"([A-Za-z0-9_\-/]+):\s+(.+)", header)
    if not match:
        _die(
            f"commit message header must match  '<module>: <description>'\n"
            f"  got: {header!r}\n"
            f"  module must consist of [A-Za-z0-9_\\-/] followed by ': <description>'"
        )

    module = match.group(1)
    description = match.group(2).strip()

    if not description:
        _die("commit message description must not be empty")

    # General modules are always valid.
    if module in GENERAL_MODULES:
        sys.exit(0)

    # Resolve repo root and crates.
    repo_root = find_git_root()
    if repo_root is None:
        _die("could not locate git repository root")

    cargo_toml = os.path.join(repo_root, "Cargo.toml")
    crates = parse_cargo_members(cargo_toml)

    # Explicit rejection: writing "src" after the crate name is forbidden.
    for crate in crates:
        if module.startswith(crate + "/src/") or module == crate + "/src":
            _die(
                f"module '{module}' contains 'src/' after the crate name\n"
                f"  Omit the src/ directory: use '{crate}/{module[len(crate)+5:]}' instead"
            )

    if module_path_exists(module, repo_root, crates):
        sys.exit(0)



    crates_str = ", ".join(crates) if crates else "(none found)"
    _die(
        f"unknown module '{module}'\n"
        f"\n"
        f"  The module must be one of:\n"
        f"    • a general module: {', '.join(sorted(GENERAL_MODULES))}\n"
        f"    • an existing directory in the repo  (e.g. core/compress/stages)\n"
        f"    • a path to a .rs file without the extension\n"
        f"      (e.g. cli/commands/image  →  cli/src/commands/image.rs)\n"
        f"\n"
        f"  Workspace crates (src/ may be omitted): {crates_str}"
        f"\n"
        f"  See more at CONTRIBUTING.md → Commit message format"
    )


def _die(msg: str) -> None:
    print(f"commit-msg: ERROR: {msg}", file=sys.stderr)
    sys.exit(1)


if __name__ == "__main__":
    main()
