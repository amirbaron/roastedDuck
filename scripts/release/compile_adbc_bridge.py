#!/usr/bin/env python3
"""Build the private ADBC test bridge separately, then restore the public build."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--target", required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
# The manylinux container runs as root over the runner-owned checkout. Trust
# only this explicitly selected source directory, without changing Git config.
# Resolve provenance before spending time compiling the test-only bridge.
source_commit = subprocess.check_output(
    ["git", "-c", "safe.directory=" + str(ROOT), "rev-parse", "HEAD"], cwd=ROOT, text=True
).strip()
args.out.mkdir(parents=True, exist_ok=False)
library = "libduckdb.dylib" if "apple" in args.target else "libduckdb.so"
command = ["cargo", "build", "--locked", "--release", "-j", "1", "--target", args.target, "-p", "roastedduck-capi"]
try:
    subprocess.run([*command, "--features", "adbc-test-api"], cwd=ROOT, check=True)
    destination = args.out / library
    shutil.copy2(ROOT / "target" / args.target / "release" / library, destination)
    if "apple" in args.target:
        subprocess.run(["install_name_tool", "-id", "@rpath/libduckdb.dylib", str(destination)], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(destination)], check=True)
        subprocess.run(["codesign", "--verify", "--strict", str(destination)], check=True)
    with destination.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    (args.out / "provenance.json").write_text(json.dumps({"test_only": True, "feature": "adbc-test-api",
        "library": library, "sha256": digest, "target": args.target,
        "source_commit": source_commit}, indent=2) + "\n")
finally:
    subprocess.run(command, cwd=ROOT, check=True)
