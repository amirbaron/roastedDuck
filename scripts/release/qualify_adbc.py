#!/usr/bin/env python3
"""Run all 64 upstream ADBC cases with the authenticated, isolated test bridge."""
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
(args.out / "tmp").mkdir()

def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

assert sha(args.library) == args.expected_sha256
env = dict(os.environ, REF=str(args.source.resolve()), ADBC_TEST_LIB_DIR=str(args.library.parent),
           ADBC_TEST_OUTPUT_DIR=str(args.out / "harness"), TMPDIR=str(args.out / "tmp"), TZ="UTC", ADBC_TEST_BACKEND="rust")
env.pop("LD_LIBRARY_PATH", None)
env.pop("LD_PRELOAD", None)
env["DYLD_PRINT_LIBRARIES" if sys.platform == "darwin" else "LD_DEBUG"] = "1" if sys.platform == "darwin" else "libs"
with (args.out / "build-run.log").open("w") as log:
    run = subprocess.run(["bash", str(ROOT / "scripts/adbc-tests/run.sh")], env=env, stdout=log, stderr=subprocess.STDOUT)
text = (args.out / "harness/test.log").read_text()
match = re.search(r"All tests passed \((\d+) assertions? in (\d+) test cases?\)", text)
assert run.returncode == 0 and match and (int(match[1]), int(match[2])) == (3040, 64)
assert text.count("Verified selected DuckDB image: " + str(args.library)) == 2
binary = args.out / "harness/adbc-tests"
listed = subprocess.run([str(binary), "--list-test-names-only"], cwd=args.source,
                        env=dict(env, DUCKDB_INSTALL_LIB=str(args.library)), capture_output=True, text=True)
ids = listed.stdout.strip().splitlines()
assert len(ids) == len(set(ids)) == 64
assert sha(args.library) == args.expected_sha256
(args.out / "verification.json").write_text(json.dumps({"passed": True, "test_bridge_sha256": args.expected_sha256,
    "test_only_bridge": True, "cases": 64, "assertions": 3040, "case_ids": ids,
    "harness_sha256": sha(binary)}, indent=2) + "\n")
print("ADBC: 64 cases passed against", args.library)
