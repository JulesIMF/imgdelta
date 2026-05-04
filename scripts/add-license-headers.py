#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 JulesIMF
#
# image-delta — incremental disk-image compression toolkit
# One-shot script: prepend license headers to all project source files
"""
Usage:
    python3 scripts/add-license-headers.py [--dry-run]

Adds a 5-line SPDX license header to every .rs / .toml / .sh / .sql / .env*
file that does not already contain "SPDX-License-Identifier".

Pass --dry-run to print which files would be modified without changing them.
"""

import os
import sys

DRY_RUN = "--dry-run" in sys.argv

# ---------------------------------------------------------------------------
# Comment styles per extension
# ---------------------------------------------------------------------------
STYLES = {
    ".rs": ("// ", "//"),
    ".toml": ("# ", "#"),
    ".sh": ("# ", "#"),
    ".sql": ("-- ", "--"),
    ".env": ("# ", "#"),
}

PROJECT = "image-delta — incremental disk-image compression toolkit"
AUTHOR = "JulesIMF"
YEAR = "2026"
LICENSE = "MIT OR Apache-2.0"

# ---------------------------------------------------------------------------
# Header generation
# ---------------------------------------------------------------------------


def make_header(ext: str, description: str) -> str:
    line, blank = STYLES[ext]
    return (
        f"{line}SPDX-License-Identifier: {LICENSE}\n"
        f"{line}Copyright (c) {YEAR} {AUTHOR}\n"
        f"{blank}\n"
        f"{line}{PROJECT}\n"
        f"{line}{description}\n"
    )


def already_headered(content: str) -> bool:
    return "SPDX-License-Identifier" in content[:400]


def apply_header(path: str, rel: str) -> bool:
    """Return True if the file was (or would be) modified."""
    _, ext = os.path.splitext(path)
    basename_name = os.path.basename(path)
    if basename_name.startswith(".env"):
        ext = ".env"
    if ext not in STYLES:
        return False

    description = f"{os.path.basename(path)} — see module docs"

    with open(path, "r", encoding="utf-8", errors="replace") as f:
        content = f.read()

    if already_headered(content):
        return False

    header = make_header(ext, description)

    # For .sh: keep shebang as first line
    if ext == ".sh" and content.startswith("#!"):
        newline_pos = content.index("\n")
        shebang = content[: newline_pos + 1]
        rest = content[newline_pos + 1 :]
        new_content = shebang + header + "\n" + rest
    else:
        new_content = header + "\n" + content

    if DRY_RUN:
        print(f"[dry-run] would header: {rel}")
        return True

    with open(path, "w", encoding="utf-8") as f:
        f.write(new_content)
    print(f"headered: {rel}")
    return True


# ---------------------------------------------------------------------------
# Walk the repo
# ---------------------------------------------------------------------------
EXTENSIONS = set(STYLES.keys())


def walk(root: str):
    modified = 0
    for dirpath, dirnames, filenames in os.walk(root):
        # Skip build artefacts and hidden tool caches
        dirnames[:] = [
            d
            for d in dirnames
            if d not in {"target", ".git", "node_modules", "__pycache__", ".venv"}
        ]
        for name in filenames:
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, root)
            _, ext = os.path.splitext(name)
            if name.startswith(".env"):
                ext = ".env"
            if ext in EXTENSIONS:
                if apply_header(full, rel):
                    modified += 1
    return modified


if __name__ == "__main__":
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    n = walk(repo_root)
    action = "would modify" if DRY_RUN else "modified"
    print(f"\nDone — {action} {n} file(s).")
