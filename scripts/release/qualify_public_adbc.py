#!/usr/bin/env python3
"""Exercise public ADBC and Arrow ownership on the exact feature-free shipping SDK."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--library", type=Path, required=True)
parser.add_argument("--expected-sha256", required=True)
parser.add_argument("--headers", type=Path, required=True, help="The actual SDK's public include directory")
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.library = args.library.resolve()
args.out = args.out.resolve()
args.out.mkdir(parents=True, exist_ok=False)
def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()
assert sha(args.library) == args.expected_sha256
binary = args.out / "public-adbc"
assert sha(args.headers / "adbc.h") == sha(ROOT / "include/adbc.h")
command = ["cc", "-std=c11", str(ROOT / "scripts/release/smoke_adbc.c"), "-I" + str(args.headers.resolve()),
           str(args.library), "-Wl,-rpath," + str(args.library.parent), "-o", str(binary)]
if sys.platform == "darwin":
    command += ["-Wl,-headerpad_max_install_names"]
else:
    command += ["-ldl"]
with (args.out / "build.log").open("w") as log:
    subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
if sys.platform == "darwin":
    identifier = subprocess.check_output(["otool", "-D", str(args.library)], text=True).splitlines()[1].strip()
    subprocess.run(["install_name_tool", "-change", identifier, str(args.library), str(binary)], check=True, capture_output=True)
    subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(binary)], check=True, capture_output=True)
env = dict(os.environ, TZ="UTC", TMPDIR=str(args.out))
for key in ["LD_LIBRARY_PATH", "LD_PRELOAD", "DYLD_LIBRARY_PATH", "DYLD_FRAMEWORK_PATH", "DYLD_INSERT_LIBRARIES"]:
    env.pop(key, None)
run = subprocess.run([str(binary)], env=env, capture_output=True, text=True, timeout=60)
(args.out / "run.log").write_text(run.stdout + run.stderr)
assert run.returncode == 0 and "public_adbc_pass" in run.stdout
selected = [line.split("=", 1)[1] for line in run.stdout.splitlines() if line.startswith("selected_library=")]
assert len(selected) == 1 and Path(selected[0]).resolve() == args.library
assert sha(args.library) == args.expected_sha256
(args.out / "verification.json").write_text(json.dumps({"passed": True, "native_sha256": args.expected_sha256,
    "library": str(args.library), "public_header_sha256": sha(args.headers / "adbc.h"), "test_only_bridge": False, "adbc_versions": ["1.0", "1.1"],
    "checks": ["initialize", "connect", "query", "arrow-schema", "arrow-batch", "stream-end", "independent-arrow-ownership", "release"],
    "harness_sha256": sha(binary)}, indent=2) + "\n")
print("Public feature-free ADBC 1.0/1.1 and Arrow ownership passed")
