#!/usr/bin/env python3
"""Record immutable source, lockfile, toolchain and build-policy identities."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
overrides = {key: value for key, value in os.environ.items() if key.startswith("CARGO_PROFILE_RELEASE_") and value}
assert not overrides, "Undeclared Cargo release-profile overrides: " + repr(overrides)
subprocess.run(["git", "diff", "--exit-code", "HEAD"], cwd=ROOT, check=True, capture_output=True)
paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")
inputs = {}
for name in paths:
    if not name:
        continue
    path = ROOT / name
    inputs[name] = hashlib.sha256(path.read_bytes()).hexdigest()
report = {
    "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
    "tree": subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], cwd=ROOT, text=True).strip(),
    "release": json.loads((ROOT / "release.json").read_text()),
    "source_sha256": inputs,
    "cargo_profile": "release (repository-defined; no LTO or optimization overrides)",
    "cargo_build_jobs": "1",
    "github_run_id": os.environ.get("GITHUB_RUN_ID"),
    "github_repository": os.environ.get("GITHUB_REPOSITORY"),
}
args.out.parent.mkdir(parents=True, exist_ok=True)
args.out.write_text(json.dumps(report, indent=2) + "\n")
print("Recorded", len(inputs), "source inputs at", report["commit"])
