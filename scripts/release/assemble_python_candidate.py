#!/usr/bin/env python3
"""Gather the exact qualified platform wheels and tested source distribution."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--input", type=Path, required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
for platform in ["macos-arm64", "linux-arm64", "linux-amd64"]:
    directory = args.input / ("candidate-" + platform)
    inputs = json.loads((directory / "receipts/build-inputs.json").read_text())
    assert inputs["commit"] == commit, (platform, inputs["commit"], commit)
    wheels = list((directory / "wheelhouse").glob("*.whl"))
    assert len(wheels) == 5
    for wheel in wheels:
        destination = args.out / wheel.name
        assert not destination.exists()
        shutil.copy2(wheel, destination)
sdists = list((args.input / "candidate-sdist").rglob("roastedduck-*.tar.gz"))
assert len(sdists) == 1
assert json.loads((args.input / "candidate-sdist/build-inputs.json").read_text())["commit"] == commit
shutil.copy2(sdists[0], args.out / sdists[0].name)
subprocess.run(["python", str(ROOT / "scripts/release/inspect_distribution.py"), str(args.out),
                "--complete", "--report", str(args.out / "python-distributions.json")], check=True)
manifest = json.loads((args.out / "python-distributions.json").read_text())
manifest["source_commit"] = commit
(args.out / "python-distributions.json").write_text(json.dumps(manifest, indent=2) + "\n")
(args.out / "SHA256SUMS").write_text("".join(
    hashlib.sha256(path.read_bytes()).hexdigest() + "  " + path.name + "\n"
    for path in sorted(args.out.iterdir()) if path.is_file()
))
print("Assembled 15 qualified wheels and one source distribution")
