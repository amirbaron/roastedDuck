#!/usr/bin/env python3
"""Build and run all pinned Go cases against one explicitly authenticated SDK."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", type=Path, required=True)
parser.add_argument("--library", type=Path, required=True)
parser.add_argument("--expected-sha256", required=True)
parser.add_argument("--out", type=Path, required=True)
parser.add_argument("--go", default="go")
args = parser.parse_args()
args.out = args.out.resolve()
args.library = args.library.resolve()
args.out.mkdir(parents=True, exist_ok=False)
pin = json.loads((ROOT / "go/upstream.json").read_text())


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


assert sha(args.library) == args.expected_sha256
source = args.out / "source"
shutil.copytree(args.source, source)
assert {str(p.relative_to(source)): sha(p) for p in source.rglob("*") if p.is_file()} == pin["files"]
for name in ["tmp", "cache", "modules", "gopath", "telemetry"]:
    (args.out / name).mkdir()
env = dict(os.environ, CGO_ENABLED="1", GOTOOLCHAIN="local", GOMAXPROCS="2", GOFLAGS="",
           GOCACHE=str(args.out / "cache"), GOMODCACHE=str(args.out / "modules"), GOPATH=str(args.out / "gopath"),
           GOTMPDIR=str(args.out / "tmp"), TMPDIR=str(args.out / "tmp"), TZ="UTC", GOTELEMETRY="off",
           CGO_LDFLAGS=f'"{args.library}" "-L{args.library.parent}" "-Wl,-rpath,{args.library.parent}"', CGO_LDFLAGS_ALLOW=".*")
env.pop("LD_LIBRARY_PATH", None)
binary = args.out / "go-tests"
command = [args.go, "test", "-c", "-tags=duckdb_use_lib,duckdb_arrow", "-p", "1", "-o", str(binary), "."]
with (args.out / "build.log").open("w") as log:
    subprocess.run(command, cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
header = args.out / "modules" / ("github.com/duckdb/duckdb-go-bindings@" + pin["bindings_version"]) / "include/duckdb.h"
assert sha(header) == pin["bindings_header_sha256"]
if sys.platform == "darwin":
    identifier = subprocess.check_output(["otool", "-D", str(args.library)], text=True).splitlines()[1].strip()
    subprocess.run(["install_name_tool", "-change", identifier, str(args.library), str(binary)], check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(binary)], check=True)
    env["DYLD_PRINT_LIBRARIES"] = "1"
else:
    dependencies = subprocess.check_output(["ldd", str(binary)], env=env, text=True)
    assert str(args.library) in dependencies and "not found" not in dependencies
    (args.out / "dependencies.txt").write_text(dependencies)
    env["LD_DEBUG"] = "libs"
binary_hash = sha(binary)
listed = subprocess.run([str(binary), "-test.list", "."], cwd=source, env=env, text=True, capture_output=True, check=True)
top_level = [line for line in listed.stdout.splitlines() if line.startswith("Test")]
assert len(top_level) == 255, len(top_level)
with (args.out / "tests.log").open("w") as stdout, (args.out / "loader.log").open("w") as stderr:
    result = subprocess.run([str(binary), "-test.v=test2json", "-test.parallel=1", "-test.timeout=20m"], cwd=source, env=env,
                            stdout=stdout, stderr=stderr, timeout=1250)
text = (args.out / "tests.log").read_text()
started, finished = [], {}
for line in text.splitlines():
    line = line.lstrip("\x16 \t")
    beginning = re.match(r"^=== RUN\s+(.+)$", line)
    ending = re.match(r"^--- (PASS|FAIL|SKIP): (\S+) \(", line)
    if beginning:
        started.append(beginning[1])
    if ending:
        assert ending[2] not in finished
        finished[ending[2]] = ending[1].lower()
assert result.returncode == 0 and len(finished) == 652
assert set(started) == set(finished) and set(top_level) <= set(finished)
assert all(value == "pass" for value in finished.values())
assert str(args.library) in (args.out / "loader.log").read_text()
assert sha(args.library) == args.expected_sha256 and sha(binary) == binary_hash
for name, expected in pin["files"].items():
    assert sha(source / name) == expected, "Pinned Go source/fixture changed: " + name
(args.out / "verification.json").write_text(json.dumps({
    "passed": True, "cases": len(finished), "counts": dict(Counter(finished.values())), "case_ids": sorted(finished),
    "top_level": top_level, "native_sha256": args.expected_sha256, "library": str(args.library),
    "harness_sha256": binary_hash, "source_commit": pin["commit"], "source_built_harness": True,
}, indent=2) + "\n")
print("Go:", len(finished), "cases passed against", args.library)
