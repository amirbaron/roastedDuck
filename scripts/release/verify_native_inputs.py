#!/usr/bin/env python3
"""Verify native inputs against the producer's recorded source and file hashes."""
import argparse
import hashlib
import json
from pathlib import Path
from archive_native_sdk import expected_layout

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--native", type=Path, required=True)
parser.add_argument("--build-inputs", type=Path, required=True)
parser.add_argument("--expected-commit", required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
root = args.native.resolve()
build = json.loads(args.build_inputs.read_text())
provenance = json.loads((root / "build-provenance.json").read_text())
assert build["commit"] == provenance["source_commit"] == args.expected_commit
assert provenance["export_count"] == 547 and provenance["adbc_test_api"] is False
actual_files = {str(file.relative_to(root)) for file in root.rglob("*") if file.is_file() and not file.is_symlink()}
assert actual_files == set(provenance["files_sha256"]) | {"build-provenance.json"}, "Native artifact file set changed"
for name, expected in provenance["files_sha256"].items():
    file = (root / name).resolve()
    assert file.is_relative_to(root) and file.is_file(), name
    with file.open("rb") as stream:
        assert hashlib.file_digest(stream, "sha256").hexdigest() == expected, name
executable, links = expected_layout(provenance)
assert (root / "sdk" / executable).is_file() and not (root / "sdk" / executable).is_symlink()
actual_links = {str(path.relative_to(root / "sdk")): str(path.readlink())
                for path in (root / "sdk").rglob("*") if path.is_symlink()}
assert actual_links == links
args.out.parent.mkdir(parents=True, exist_ok=True)
args.out.write_text(json.dumps({"passed": True, "source_commit": args.expected_commit,
                               "producer_manifest_sha256": hashlib.sha256((root / "build-provenance.json").read_bytes()).hexdigest(),
                               "files_verified": len(provenance["files_sha256"]),
                               "library_sha256": {name: digest for name, digest in provenance["files_sha256"].items() if name.startswith("sdk/lib/")}}, indent=2) + "\n")
