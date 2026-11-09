#!/usr/bin/env python3
"""Compile release-profile security/persistence harnesses for later native execution."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--target", required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
groups = [
    ("crypto", ["-p", "rustdb-crypto", "--lib"], []),
    ("extension-signatures", ["-p", "rustdb-capi", "--lib"], ["extension_loader::tests"]),
    ("parquet-encryption", ["-p", "rustdb", "--test", "parquet_encryption"], []),
    ("persistence", ["-p", "rustdb", "--test", "persistence"], []),
]
records = []
for name, selection, filters in groups:
    command = ["cargo", "test", "--locked", "--release", "--no-run", "-j", "1", "--target", args.target,
               "--message-format=json-render-diagnostics", *selection]
    with (args.out / (name + "-compile.jsonl")).open("w") as output:
        subprocess.run(command, cwd=ROOT, stdout=output, check=True)
    executables = []
    for line in (args.out / (name + "-compile.jsonl")).read_text().splitlines():
        row = json.loads(line)
        if row.get("reason") == "compiler-artifact" and row.get("executable"):
            executables.append(Path(row["executable"]))
    assert len(executables) == 1, (name, executables)
    destination = args.out / name
    shutil.copy2(executables[0], destination)
    if sys.platform == "darwin":
        subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(destination)], check=True)
        subprocess.run(["codesign", "--verify", "--strict", str(destination)], check=True)
    with destination.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    records.append({"name": name, "sha256": digest, "filters": filters, "target": args.target})
(args.out / "manifest.json").write_text(json.dumps(records, indent=2) + "\n")
