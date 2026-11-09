#!/usr/bin/env python3
"""Build the pinned Swift tests and prove all 133 cases use the selected SDK."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/swift-tests"))
from build import project_macho, verify_inputs

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", type=Path, required=True)
parser.add_argument("--library", type=Path, required=True)
parser.add_argument("--expected-sha256", required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out = args.out.resolve()
args.library = args.library.resolve()
args.out.mkdir(parents=True, exist_ok=False)

def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

assert sha(args.library) == args.expected_sha256
package = args.out / "package"
subprocess.run([sys.executable, str(ROOT / "scripts/swift-tests/prepare.py"), "--source", str(args.source),
                "--library", str(args.library), "--engine", "rust", "--out", str(package),
                "--dedicated-test-fixtures", "--identity-test"], check=True, capture_output=True)
directories = {name: args.out / name for name in ["build", "cache", "config", "security", "tmp", "module-cache", "fixtures"]}
for path in directories.values():
    path.mkdir()
env = dict(os.environ, TMPDIR=str(directories["tmp"]), TZ="UTC",
           CLANG_MODULE_CACHE_PATH=str(directories["module-cache"]), SWIFTPM_MODULECACHE_OVERRIDE=str(directories["module-cache"]),
           ROASTEDDUCK_SWIFT_TEST_DIRECTORY=str(directories["fixtures"]), ROASTEDDUCK_SWIFT_EXPECTED_LIBRARY=str(args.library),
           ROASTEDDUCK_SWIFT_IDENTITY_REPORT=str(args.out / "identity.json"))
env.pop("LD_LIBRARY_PATH", None)
common = ["--package-path", str(package), "--scratch-path", str(directories["build"]),
          "--cache-path", str(directories["cache"]), "--config-path", str(directories["config"]),
          "--security-path", str(directories["security"]), "--jobs", "1"]
if sys.platform == "darwin":
    common += ["--triple", "arm64-apple-macosx12.0"]
with (args.out / "build.log").open("w") as log:
    subprocess.run(["swift", "build", *common, "--build-tests"], env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
if sys.platform == "darwin":
    identifier = subprocess.check_output(["otool", "-D", str(args.library)], text=True).splitlines()[1].strip()
    for binary in sorted(directories["build"].rglob("*")):
        if not binary.is_file() or binary.is_symlink() or not project_macho(binary):
            continue
        dependencies = subprocess.check_output(["otool", "-L", str(binary)], text=True)
        if identifier != str(args.library) and identifier in dependencies:
            subprocess.run(["install_name_tool", "-change", identifier, str(args.library), str(binary)], check=True, capture_output=True)
        subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(binary)], check=True, capture_output=True)
    for bundle in directories["build"].rglob("*.xctest"):
        if bundle.is_dir() and not bundle.is_symlink():
            subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(bundle)], check=True, capture_output=True)
    env["DYLD_PRINT_LIBRARIES"] = "1"
else:
    env["LD_DEBUG"] = "libs"
artifacts = {str(path): sha(path) for path in directories["build"].rglob("*") if path.is_file() and not path.is_symlink()}
with (args.out / "run.log").open("w") as log:
    run = subprocess.run(["swift", "test", *common, "--skip-build", "--no-parallel", "--xunit-output", str(args.out / "junit.xml")],
                         env=env, stdout=log, stderr=subprocess.STDOUT, timeout=600)
text = (args.out / "run.log").read_text()
started = re.findall(r"Test Case '([^']+)' started", text)
passed = re.findall(r"Test Case '([^']+)' passed", text)
identity_cases = [name for name in started if "LibraryIdentityTests" in name]
assert run.returncode == 0 and len(started) == len(set(started)) == len(passed) == 133
assert set(started) == set(passed) and len(identity_cases) == 1
identity = json.loads((args.out / "identity.json").read_text())
assert identity["function_image"] == str(args.library) and identity["engine_images"] == [str(args.library)]
assert sha(args.library) == args.expected_sha256
# Runtime execution may update SwiftPM's bookkeeping, but executable image bytes must stay fixed.
for name, digest in artifacts.items():
    path = Path(name)
    if path.suffix in {".so", ".dylib"} or os.access(path, os.X_OK):
        assert sha(path) == digest
preparation = json.loads((package / "preparation.json").read_text())
verify_inputs(package, preparation, json.loads((ROOT / "swift/upstream.json").read_text()))
(args.out / "verification.json").write_text(json.dumps({"passed": True, "native_sha256": args.expected_sha256,
    "cases": 133, "upstream_cases": 132, "identity_cases": 1, "case_ids": sorted(started),
    "identity": identity, "source_built_harness": True}, indent=2) + "\n")
print("Swift: 133 cases passed against", args.library)
