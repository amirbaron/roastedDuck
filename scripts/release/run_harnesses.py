#!/usr/bin/env python3
"""Run the exact compiled harnesses on the native target; preserve every outcome."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--input", type=Path, required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
records = json.loads((args.input / "manifest.json").read_text())
results = []
for record in records:
    path = (args.input / record["name"]).resolve()
    with path.open("rb") as stream:
        assert hashlib.file_digest(stream, "sha256").hexdigest() == record["sha256"]
    target = record["target"]
    assert ("aarch64" in target) == (platform.machine() in {"arm64", "aarch64"}), (target, platform.machine())
    temporary = (args.out / (record["name"] + "-tmp")).resolve()
    temporary.mkdir()
    env = dict(os.environ, TMPDIR=str(temporary), TZ="UTC")
    log = args.out / (record["name"] + ".log")
    with log.open("w") as output:
        result = subprocess.run([str(path), *record["filters"], "--test-threads=1", "--nocapture"],
                                env=env, stdout=output, stderr=subprocess.STDOUT)
    text = log.read_text()
    counts = re.search(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored", text)
    assert counts and int(counts[2]) > 0, (record["name"], text[-2000:])
    results.append({"name": record["name"], "exit_code": result.returncode,
                    "passed": int(counts[2]), "failed": int(counts[3]), "ignored": int(counts[4]),
                    "binary_sha256": record["sha256"], "native_machine": platform.machine()})
    with path.open("rb") as stream:
        assert hashlib.file_digest(stream, "sha256").hexdigest() == record["sha256"]
    (args.out / "summary.json").write_text(json.dumps(results, indent=2) + "\n")
assert all(r["exit_code"] == 0 and r["failed"] == 0 for r in results)
print("Native security/persistence harnesses passed", sum(r["passed"] for r in results), "cases")
