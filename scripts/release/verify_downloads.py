#!/usr/bin/env python3
"""Verify a downloaded subset against the release manifest; fail on unknown files."""
import hashlib
from pathlib import Path
import sys

root = Path(sys.argv[1])
manifest = {}
for line in (root / "SHA256SUMS").read_text().splitlines():
    checksum, name = line.split(maxsplit=1)
    manifest[name.lstrip("*")] = checksum
checked = 0
for path in root.iterdir():
    if path.name == "SHA256SUMS":
        continue
    assert path.is_file() and path.name in manifest, f"Unlisted download: {path.name}"
    with path.open("rb") as stream:
        assert hashlib.file_digest(stream, "sha256").hexdigest() == manifest[path.name], path.name
    checked += 1
assert checked, "No payload downloads to verify"
print(f"Verified {checked} downloaded payloads")
