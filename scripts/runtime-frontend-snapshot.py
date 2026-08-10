#!/usr/bin/env python3
"""Canonical hash of bounded built web assets for native bundle assembly."""

from __future__ import annotations

import hashlib
import json
import pathlib
import stat
import sys


def fail(message: str) -> None:
    raise SystemExit(f"runtime frontend snapshot failed: {message}")


if len(sys.argv) != 3:
    fail("usage: runtime-frontend-snapshot.py ASSET_ROOT OUTPUT_MANIFEST")
root = pathlib.Path(sys.argv[1]).resolve()
output = pathlib.Path(sys.argv[2])
if not root.is_dir() or root.is_symlink() or not output.is_absolute():
    fail("asset root or output manifest is invalid")
records: list[dict[str, object]] = []
total = 0
for path in sorted(root.rglob('*'), key=lambda item: item.as_posix().encode('utf-8')):
    relative = path.relative_to(root)
    if path.is_symlink() or path.is_dir():
        if path.is_symlink():
            fail(f"asset graph contains a link: {relative}")
        continue
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) & 0o111:
        fail(f"asset graph contains an unsupported member: {relative}")
    if len(records) >= 256 or info.st_size > 8 * 1024 * 1024:
        fail('asset file bound exceeded')
    total += info.st_size
    if total > 32 * 1024 * 1024:
        fail('asset total bound exceeded')
    data = path.read_bytes()
    records.append({'path': relative.as_posix(), 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()})
if not records:
    fail('asset graph is empty')
canonical = json.dumps({'schemaVersion': 'migration-stream/frontend-asset-snapshot/v1', 'files': records}, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode('utf-8')
output.write_bytes(canonical)
print(hashlib.sha256(canonical).hexdigest())
