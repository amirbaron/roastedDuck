#!/usr/bin/env python3
"""Compile and execute all 155 frozen public C API cases against an exact SDK."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", type=Path, required=True)
parser.add_argument("--library", type=Path, required=True)
parser.add_argument("--expected-sha256", required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out = args.out.resolve()
args.library = args.library.resolve()
args.out.mkdir(parents=True, exist_ok=False)
for name in ["tmp", "data"]:
    (args.out / name).mkdir()

def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

assert sha(args.library) == args.expected_sha256
env = dict(os.environ, REF=str(args.source.resolve()), ROASTEDDUCK_CAPI_LIBRARY_DIR=str(args.library.parent),
           ROASTEDDUCK_CAPI_TEST_OUTPUT_DIR=str(args.out / "build"), ROASTEDDUCK_CAPI_TEST_BIN_DIR=str(args.out / "bin"),
           CAPI_TEST_DIR=str(args.out / "data"), TMPDIR=str(args.out / "tmp"), TZ="UTC",
           ROASTEDDUCK_TEST_ADHOC_SIGNING="1", DUCKDB_INSTALL_LIB=str(args.library))
env.pop("LD_LIBRARY_PATH", None)
env.pop("LD_PRELOAD", None)
if sys.platform != "darwin":
    env["LD_DEBUG"] = "libs"
with (args.out / "build-run.log").open("w") as log:
    run = subprocess.run(["bash", str(ROOT / "scripts/capi-tests/run.sh"), "--lib", "rust", "--no-build"],
                         env=env, stdout=log, stderr=subprocess.STDOUT)
assert run.returncode == 0, "C API harness failed; see build-run.log"
records = []
for path in sorted((args.out / "bin").glob("*.log")):
    if path.name.endswith((".link.log", ".build.log")):
        continue
    text = path.read_text()
    match = re.search(r"All tests passed \((\d+) assertions? in (\d+) test cases?\)", text)
    assert match and text.count("Verified selected DuckDB image: " + str(args.library)) == 2, path
    binary = path.with_suffix("")
    cases = subprocess.run([str(binary), "--list-test-names-only"], cwd=args.source, env=env,
                           capture_output=True, text=True)
    ids = cases.stdout.strip().splitlines()
    assert len(ids) == len(set(ids)) == int(match[2])
    records.append({"name": binary.name, "cases": len(ids), "assertions": int(match[1]),
                    "case_ids": ids, "harness_sha256": sha(binary)})
assert len(records) == 30 and sum(item["cases"] for item in records) == 155
assert sha(args.library) == args.expected_sha256
(args.out / "verification.json").write_text(json.dumps({"passed": True, "native_sha256": args.expected_sha256,
    "cases": 155, "assertions": sum(item["assertions"] for item in records), "records": records}, indent=2) + "\n")
print("C API: 155 cases passed against", args.library)
