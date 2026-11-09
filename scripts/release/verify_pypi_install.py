#!/usr/bin/env python3
"""Prove an ordinary PyPI install resolved the exact accepted distribution."""
import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
from urllib.parse import unquote, urlsplit

from pip_report import archive_sha256

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--pip-report", type=Path, required=True)
parser.add_argument("--manifest", type=Path, required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
manifest = json.loads(args.manifest.read_text())
report = json.loads(args.pip_report.read_text())
items = [item for item in report["install"] if item["metadata"]["name"].lower() == "roastedduck"]
assert len(items) == 1
item = items[0]
assert item["metadata"]["version"] == manifest["version"]
url = urlsplit(item["download_info"]["url"])
assert url.scheme == "https" and url.hostname == "files.pythonhosted.org", url.geturl()
filename = unquote(Path(url.path).name)
entries = {record["filename"]: record for record in manifest["distributions"]}
assert filename in entries, filename
expected = entries[filename]
assert archive_sha256(item["download_info"]["archive_info"]) == expected["sha256"]
assert importlib.metadata.version("roastedduck") == manifest["version"]
try:
    importlib.metadata.version("duckdb")
except importlib.metadata.PackageNotFoundError:
    pass
else:
    raise AssertionError("Official duckdb must not be co-installed")
import duckdb
from duckdb import _duckdb

assert _duckdb.__rust_engine__ is True
native = Path(_duckdb.__file__).resolve()
assert hashlib.sha256(native.read_bytes()).hexdigest() == expected["native_sha256"]
assert duckdb.sql("SELECT sum(i) FROM range(10) t(i)").fetchone() == (45,)
args.out.parent.mkdir(parents=True, exist_ok=True)
args.out.write_text(json.dumps({
    "passed": True, "version": manifest["version"], "filename": filename,
    "provider_url": url.geturl(), "wheel_sha256": expected["sha256"],
    "native_sha256": expected["native_sha256"], "native_path": str(native),
}, indent=2) + "\n")
print("Verified ordinary PyPI resolution, installed native identity, and SQL:", filename)
