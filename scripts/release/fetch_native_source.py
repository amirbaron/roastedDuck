#!/usr/bin/env python3
"""Fetch the immutable DuckDB 1.5.6 C API, ADBC, and Swift test inputs."""
import argparse
import hashlib
from pathlib import Path
import shutil
import tarfile
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
archive = args.out / "duckdb-v1.5.6.tar.gz"
with urllib.request.urlopen("https://codeload.github.com/duckdb/duckdb/tar.gz/refs/tags/v1.5.6", timeout=90) as response, archive.open("wb") as stream:
    shutil.copyfileobj(response, stream)
with archive.open("rb") as stream:
    assert hashlib.file_digest(stream, "sha256").hexdigest() == "1fadcbe9e69e1470f9093b6bcde08daf477d729c449e59a807f45c346622099b"
with tarfile.open(archive) as source:
    source.extractall(args.out, filter="data")
print((args.out / "duckdb-1.5.6").resolve())
