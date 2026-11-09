#!/usr/bin/env python3
"""Fetch and validate every file of the pinned public Go client source."""
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
pin = json.loads((ROOT / "go/upstream.json").read_text())
args.out.mkdir(parents=True, exist_ok=True)
archive = args.out / "go-source.tar.gz"
with urllib.request.urlopen(f'https://codeload.github.com/duckdb/duckdb-go/tar.gz/{pin["commit"]}', timeout=90) as response:
    archive.write_bytes(response.read())
assert hashlib.sha256(archive.read_bytes()).hexdigest() == pin["archive_sha256"]
with tarfile.open(archive) as source:
    source.extractall(args.out, filter="data")
directory = args.out / ("duckdb-go-" + pin["commit"])
actual = {str(p.relative_to(directory)): hashlib.sha256(p.read_bytes()).hexdigest() for p in directory.rglob("*") if p.is_file()}
assert actual == pin["files"], "Go client source/test files differ from the pin"
print(directory.resolve())
