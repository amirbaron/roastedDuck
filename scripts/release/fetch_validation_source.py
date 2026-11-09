#!/usr/bin/env python3
"""Fetch the exact public upstream Python test source pinned by this repository."""
import argparse
import hashlib
import json
from pathlib import Path
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
pin = json.loads((ROOT / "python/upstream.json").read_text())
args.out.mkdir(parents=True, exist_ok=True)
archive_path = args.out / "upstream.tar.gz"
if not archive_path.exists():
    with urllib.request.urlopen(f'https://codeload.github.com/duckdb/duckdb-python/tar.gz/{pin["commit"]}', timeout=90) as response:
        with archive_path.open("wb") as output:
            while block := response.read(1024 * 1024):
                output.write(block)
with archive_path.open("rb") as stream:
    assert hashlib.file_digest(stream, "sha256").hexdigest() == pin["archive_sha256"]
destination = args.out / f'duckdb-python-{pin["commit"]}'
if not destination.exists():
    with tarfile.open(archive_path) as archive:
        archive.extractall(args.out, filter="data")
assert (destination / "tests/fast/test_duckdb_api.py").is_file()
print(destination.resolve())
