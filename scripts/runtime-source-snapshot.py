#!/usr/bin/env python3
"""Write the canonical Git-visible runtime source snapshot and print its SHA-256.

This is build-time orchestration only. The unpacked runtime never invokes
Python, Git, Node, Rust tooling, or this script.
"""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import stat
import subprocess
import sys


def fail(message: str) -> None:
    raise SystemExit(f"runtime source snapshot failed: {message}")


if len(sys.argv) != 4:
    fail("usage: runtime-source-snapshot.py REPOSITORY_ROOT OUTPUT_MANIFEST GIT_EXECUTABLE")

root = pathlib.Path(sys.argv[1]).resolve()
output = pathlib.Path(sys.argv[2])
git_executable = pathlib.Path(sys.argv[3])
if not root.is_dir() or root.is_symlink():
    fail("repository root is not one real directory")
if not output.is_absolute():
    fail("output manifest path must be absolute")
if (
    not git_executable.is_absolute()
    or not git_executable.is_file()
    or git_executable.is_symlink()
):
    fail("Git executable is not one absolute real regular file")

inventory = subprocess.check_output(
    [
        str(git_executable),
        "-C",
        str(root),
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
    ]
).split(b"\0")
if inventory and inventory[-1] == b"":
    inventory.pop()
if not inventory:
    fail("Git-visible source inventory is empty")

records: list[dict[str, object]] = []
seen: set[str] = set()
for raw in inventory:
    relative = raw.decode("utf-8", errors="strict")
    parts = pathlib.PurePosixPath(relative).parts
    if (
        not relative
        or relative in seen
        or pathlib.PurePosixPath(relative).is_absolute()
        or any(part in ("", ".", "..") for part in parts)
        or any(ord(character) < 32 or ord(character) == 127 for character in relative)
    ):
        fail(f"unsafe or duplicate source path: {relative!r}")
    seen.add(relative)
    path = root
    for part in parts:
        path /= part
        if not path.exists() and not path.is_symlink():
            # A reviewed dirty snapshot may deliberately delete a tracked path.
            break
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            fail(f"source path contains a symlink: {relative!r}")
    else:
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode):
            fail(f"source path is not a regular file: {relative!r}")
        data = path.read_bytes()
        records.append(
            {
                "path": relative,
                "executable": os.name != "nt" and bool(stat.S_IMODE(info.st_mode) & 0o111),
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
            }
        )

records.sort(key=lambda item: str(item["path"]).encode("utf-8"))
canonical = json.dumps(
    {"schemaVersion": "migration-stream/source-snapshot-manifest/v1", "files": records},
    ensure_ascii=False,
    sort_keys=True,
    separators=(",", ":"),
).encode("utf-8")
output.write_bytes(canonical)
print(hashlib.sha256(canonical).hexdigest())
