#!/usr/bin/env python3
"""Authenticate frozen producer bytes before recovering an unstarted CI job."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from observer_protocol import ROOT, api, download_artifact

p = argparse.ArgumentParser(description=__doc__)
p.add_argument("--platform", choices=["linux-amd64", "macos-arm64"], required=True)
p.add_argument("--out", type=Path, required=True)
p.add_argument("--receipt", type=Path, required=True)
a = p.parse_args()
config = json.loads((ROOT / "scripts/release/recovery-inputs.json").read_text())
repo, producer, commit = config["repository"], config["producer_run"], config["candidate_source_commit"]
assert os.environ["GITHUB_REPOSITORY"] == repo
assert subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip() == commit
assert not subprocess.check_output(["git", "diff", "HEAD"], text=True)
tools_commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
assert tools_commit == os.environ["GITHUB_SHA"]
pin = config["artifacts"][a.platform]
record = api(f'repos/{repo}/actions/artifacts/{pin["id"]}')
assert all(record[key] == value for key, value in pin.items())
with tempfile.TemporaryDirectory(prefix="recovery-download-", dir=os.environ["RUNNER_TEMP"]) as tmp:
    verified = download_artifact(repo, record, producer, commit, a.out, Path(tmp))
a.receipt.write_text(json.dumps({"passed": True, "source_commit": commit,
    "producer_run": producer, "recovery_run": int(os.environ["GITHUB_RUN_ID"]),
    "verification_tools_commit": tools_commit, "platform": a.platform,
    "artifact": verified, "distributions_rebuilt": False}, indent=2) + "\n")
print("Authenticated original candidate", a.platform, verified["digest"], flush=True)
