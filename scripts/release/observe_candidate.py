#!/usr/bin/env python3
"""Qualify immutable producer artifacts using separately identified observer tools."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import time

from observer_protocol import ROOT, api, download_artifact, required_artifact_names, sha, source_run_evidence, verification_only_changes
from verify_qualification import verify as verify_qualification
from archive_native_sdk import verify_archive as verify_native_archive
from qualify_swift_package import verify_archive as verify_swift_archive
from verify_apple_release import verify as verify_apple

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--config", type=Path, default=ROOT / "scripts/release/observer-inputs.json")
parser.add_argument("--out", type=Path, required=True)
parser.add_argument("--apple-inputs", type=Path, required=True)
parser.add_argument("--wait", action="store_true")
args = parser.parse_args()
config = json.loads(args.config.read_text())
repository, producer_id, candidate = config["repository"], int(config["producer_run"]), config["candidate_source_commit"]
observer = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
assert os.environ.get("GITHUB_REPOSITORY", repository) == repository
assert json.loads((ROOT / "release.json").read_text())["version"] == config["version"]
changed = verification_only_changes(candidate, observer)
args.out.mkdir(parents=True, exist_ok=False)
incoming, temporary, manifests = [args.out / name for name in ["incoming", "tmp", "manifests"]]
for path in [incoming, temporary, manifests]:
    path.mkdir()

# Authenticate and validate the separately signed Apple payload before waiting.
apple_inputs = args.out / "apple-inputs"
apple_inputs.mkdir()
fetch = json.loads((args.apple_inputs / "fetch-verification.json").read_text())
assert fetch["passed"] is True and fetch["contents_executed"] is False and fetch["contents_unpacked"] is False
assert fetch["fetch_commit"] == observer and fetch["candidate_source_commit"] == candidate
assert fetch["input_release_id"] == config["apple_input_release_id"] and fetch["assets"] == config["apple_assets"]
for name, pin in config["apple_assets"].items():
    assert Path(name).name == name
    shutil.copy2(args.apple_inputs / name, apple_inputs / name)
    assert sha(apple_inputs / name) == pin["sha256"]
shutil.copy2(args.apple_inputs / "fetch-verification.json", manifests / "apple-fetch-verification.json")
apple_distribution = json.loads((apple_inputs / "apple-distribution.json").read_text())
apple_archive = apple_inputs / apple_distribution["filename"]
assert sha(apple_archive) == apple_distribution["sha256"] and apple_distribution["version"] == config["version"]
apple_root = args.out / "apple-extracted"
with tarfile.open(apple_archive) as archive:
    archive.extractall(apple_root, filter="data")
apple_proof = verify_apple(apple_root, candidate, temporary)
assert apple_proof["native_images"] == apple_distribution["native_images"]
apple_install = json.loads((apple_inputs / "final-archive-consumer.json").read_text())
assert apple_install["passed"] is True and apple_install["archive_sha256"] == sha(apple_archive)
assert apple_install["native_framework_sha256"] == apple_proof["native_images"]["macos-arm64"]
(manifests / "apple-verification.json").write_text(json.dumps(apple_proof, indent=2) + "\n")

while True:
    run = api(f"repos/{repository}/actions/runs/{producer_id}")
    assert run["head_sha"] == candidate and run["head_repository"]["full_name"] == repository
    if run["status"] == "completed":
        break
    assert args.wait, "Producer has not completed"
    jobs = api(f"repos/{repository}/actions/runs/{producer_id}/jobs?per_page=100")["jobs"]
    assert not any(job["conclusion"] in {"failure", "timed_out", "cancelled"} and job["name"] != "assemble" for job in jobs), "A producer build/runtime gate failed"
    print("Waiting for immutable producer", producer_id, flush=True)
    time.sleep(30)
evidence = source_run_evidence(repository, producer_id, candidate, manifests)
catalog = api(f"repos/{repository}/actions/runs/{producer_id}/artifacts?per_page=100")["artifacts"]
by_name = {record["name"]: record for record in catalog}
assert len(by_name) == len(catalog)
names = required_artifact_names()
artifacts = []
for name in names:
    listed = by_name[name]
    record = api(f'repos/{repository}/actions/artifacts/{int(listed["id"])}')
    assert record["id"] == listed["id"] and record["digest"] == listed["digest"]
    artifacts.append(download_artifact(repository, record, producer_id, candidate, incoming / name, temporary))
    print("Verified original artifact", name, flush=True)
assert json.loads((incoming / "security-report/report.json").read_text())["results"] == []
accepted = args.out / "python"
subprocess.run(["python", str(ROOT / "scripts/release/assemble_python_candidate.py"), "--input", str(incoming),
                "--out", str(accepted), "--source-commit", candidate], check=True)
manifest = json.loads((accepted / "python-distributions.json").read_text())
qualified = verify_qualification(incoming, manifest, candidate)
original_sdist = json.loads((incoming / "candidate-sdist/metadata.json").read_text())["distributions"]
accepted_records = {record["filename"]: record for record in manifest["distributions"]}
assert len(original_sdist) == 1 and accepted_records[original_sdist[0]["filename"]] == original_sdist[0]
for platform in qualified["native_platforms"]:
    directory = incoming / ("candidate-" + platform)
    original = json.loads((directory / "receipts/distributions.json").read_text())["distributions"]
    observed = {record["filename"]: record for record in manifest["distributions"]}
    assert all(observed[record["filename"]] == record for record in original), "Observer changed a producer distribution record"
    producer = json.loads((incoming / ("candidate-native-manifest-" + platform) / "build-provenance.json").read_text())
    archive = directory / producer["sdk_archive"]["filename"]
    verify_native_archive(archive, producer)
    output = args.out / ("sdk-" + platform)
    output.mkdir()
    shutil.copy2(archive, output / archive.name)
    shutil.copy2(incoming / ("candidate-native-manifest-" + platform) / "build-provenance.json", output / "build-provenance.json")
for platform in ["linux-arm64", "linux-amd64"]:
    record = json.loads((incoming / ("candidate-swift-manifest-" + platform) / "swift-distribution.json").read_text())
    archive = incoming / ("candidate-swift-" + platform) / record["filename"]
    verify_swift_archive(archive, record)
    output = args.out / ("swift-" + platform)
    output.mkdir()
    shutil.copy2(archive, output / archive.name)
    shutil.copy2(incoming / ("candidate-swift-manifest-" + platform) / "swift-distribution.json", output / "swift-distribution.json")

# Retain original raw receipts so the publisher can independently repeat validation.
verification_inputs = manifests / "qualification-inputs"
verification_inputs.mkdir()
for name in names:
    if name.startswith(("qualification-", "candidate-native-manifest-", "candidate-swift-manifest-")):
        shutil.copytree(incoming / name, verification_inputs / name)
shutil.copy2(accepted / "python-distributions.json", manifests / "python-distributions.json")
shutil.copy2(apple_inputs / "apple-distribution.json", manifests / "apple-distribution.json")
shutil.copy2(apple_inputs / "final-archive-consumer.json", manifests / "apple-final-archive-consumer.json")
shutil.copy2(incoming / "security-report/report.json", manifests / "dependency-scan.json")
(manifests / "qualification-verification.json").write_text(json.dumps(qualified, indent=2) + "\n")
evidence.update(verification_tools_commit=observer, observer_run=int(os.environ["GITHUB_RUN_ID"]),
                verification_only_changed_paths=changed, producer_artifacts=artifacts,
                python_manifest_sha256=sha(accepted / "python-distributions.json"),
                apple_archive_sha256=sha(apple_archive), apple_input_assets=config["apple_assets"],
                passed=True, distributions_rebuilt=False, distribution_bytes_changed=False,
                observer_scope="Complete 15-cell, native, crypto, client, archive and signed Apple validation of original producer bytes; original producer conclusion retained")
(manifests / "observer-verification.json").write_text(json.dumps(evidence, indent=2) + "\n")
print("Complete observer acceptance passed for producer", candidate, "using tools", observer, flush=True)
