#!/usr/bin/env python3
"""Fetch three pinned draft assets; never unpack or execute their contents."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--config", type=Path, required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
config = json.loads(args.config.read_text())
repository = config["repository"]
assert repository == os.environ["GITHUB_REPOSITORY"]
assert re.fullmatch(r"\d+\.\d+\.\d+(?:\.post[1-9]\d*)?", config["version"])
expected_names = {"apple-distribution.json", "final-archive-consumer.json", "roastedduck-" + config["version"] + "-swift-apple.tar.gz"}
assert set(config["apple_assets"]) == expected_names
release = json.loads(subprocess.check_output(["gh", "api", f'repos/{repository}/releases/{int(config["apple_input_release_id"])}']))
assert release["draft"] is True and release["target_commitish"] == config["candidate_source_commit"]
available = {asset["id"]: asset for asset in release["assets"]}
args.out.mkdir(parents=True, exist_ok=False)
for name, pin in config["apple_assets"].items():
    assert Path(name).name == name and re.fullmatch(r"[0-9a-f]{64}", pin["sha256"])
    assert 0 < pin["bytes"] <= 100 * 1024 * 1024
    asset = available[pin["id"]]
    assert asset["name"] == name and asset["digest"] == "sha256:" + pin["sha256"] and asset["size"] == pin["bytes"]
    destination = args.out / name
    with destination.open("wb") as output:
        subprocess.run(["gh", "api", "-H", "Accept: application/octet-stream", f'repos/{repository}/releases/assets/{int(pin["id"])}'], stdout=output, check=True)
    with destination.open("rb") as stream:
        assert hashlib.file_digest(stream, "sha256").hexdigest() == pin["sha256"]
    assert destination.stat().st_size == pin["bytes"]
(args.out / "fetch-verification.json").write_text(json.dumps({"passed": True,
    "fetch_commit": os.environ["GITHUB_SHA"], "candidate_source_commit": config["candidate_source_commit"],
    "input_release_id": config["apple_input_release_id"], "assets": config["apple_assets"],
    "contents_executed": False, "contents_unpacked": False}, indent=2) + "\n")
print("Fetched and hash-verified three pinned draft inputs without executing or unpacking them")
