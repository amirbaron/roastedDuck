#!/usr/bin/env python3
"""Extract a source distribution outside the checkout and verify local dependency closure."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("archive", type=Path)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
assert not args.out.resolve().is_relative_to(ROOT), "Source-rebuild directory must be outside the checkout"
with tarfile.open(args.archive) as archive:
    archive.extractall(args.out, filter="data")
roots = [p for p in args.out.iterdir() if p.is_dir()]
assert len(roots) == 1
source = roots[0].resolve()
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=source))
for package in metadata["packages"]:
    if package["source"] is None:
        assert Path(package["manifest_path"]).resolve().is_relative_to(source), package["manifest_path"]
for package in metadata["packages"]:
    for dependency in package["dependencies"]:
        if dependency.get("path"):
            assert Path(dependency["path"]).resolve().is_relative_to(source), dependency
report = {"archive": args.archive.name, "source_root": str(source),
          "local_packages": [p["name"] for p in metadata["packages"] if p["source"] is None],
          "local_dependency_closure_verified": True}
(args.out / "closure.json").write_text(json.dumps(report, indent=2) + "\n")
if "GITHUB_OUTPUT" in os.environ:
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write("source_root=" + str(source) + "\n")
print(source)
