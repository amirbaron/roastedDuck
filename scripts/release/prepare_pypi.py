#!/usr/bin/env python3
"""Download a publicly released, fully accepted candidate for trusted publishing.

Only the exact wheels and sdist listed in an evidence-backed launch acceptance
manifest can cross this gate. Historical payload repackaging cannot satisfy it.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tarfile
import urllib.error
import urllib.request

from verify_qualification import verify as verify_qualification
from archive_native_sdk import verify_archive
from verify_apple_release import verify as verify_apple, sha

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--tag", required=True)
parser.add_argument("--out", type=Path, required=True)
parser.add_argument("--receipts", type=Path, required=True)
args = parser.parse_args()
assert re.fullmatch(r"v\d+\.\d+\.\d+(?:\.post[1-9]\d*)?", args.tag)
version = args.tag[1:]
args.out.mkdir(parents=True, exist_ok=False)
args.receipts.mkdir(parents=True, exist_ok=True)
release = json.loads(subprocess.check_output(["gh", "release", "view", args.tag, "--json", "isDraft,tagName,assets,url"]))
assert not release["isDraft"] and release["tagName"] == args.tag
subprocess.run(["gh", "release", "download", args.tag, "--dir", str(args.receipts),
                "--pattern", "launch-acceptance.json", "--pattern", "python-distributions.json", "--pattern", "apple-distribution.json", "--pattern", "SHA256SUMS"], check=True)
acceptance = json.loads((args.receipts / "launch-acceptance.json").read_text())
manifest = json.loads((args.receipts / "python-distributions.json").read_text())
assert acceptance["version"] == manifest["version"] == version
assert acceptance["ready_for_pypi"] is True
assert acceptance["known_dependency_advisories_unresolved"] == []
required = {"source_build", "python_15_cells", "sdist_rebuild", "crypto_interoperability", "capi_adbc", "go", "swift", "apple_simulator", "final_artifact_integrity"}
assert set(acceptance["gates"]) >= required
assert all(acceptance["gates"][name]["passed"] is True for name in required)
subprocess.run(["git", "fetch", "--depth=1", "origin", "refs/tags/" + args.tag + ":refs/tags/" + args.tag], check=True)
current = subprocess.check_output(["git", "rev-parse", args.tag + "^{commit}"], text=True).strip()
assert current == acceptance["release_commit"]
assert acceptance["source_build_commit"] == current
assert subprocess.check_output(["git", "rev-parse", args.tag + "^{commit}"], text=True).strip() == current
assert acceptance["required_successful_runs"]
assert acceptance["source_build_run"] in acceptance["required_successful_runs"]
for run_id in acceptance["required_successful_runs"]:
    run = json.loads(subprocess.check_output(["gh", "run", "view", str(int(run_id)), "--json", "status,conclusion,headSha,url"]))
    assert run["status"] == "completed" and run["conclusion"] == "success", run
    assert run["headSha"] in acceptance["verified_source_commits"], run
    if run_id == acceptance["source_build_run"]:
        assert run["headSha"] == current, "Build run must belong to the exact tagged source"
        repository = subprocess.check_output(["gh", "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"], text=True).strip()
        details = json.loads(subprocess.check_output(["gh", "api", f"repos/{repository}/actions/runs/{int(run_id)}"]))
        assert details["path"].split("@", 1)[0] == ".github/workflows/build-candidate.yml"
        assert details["head_repository"]["full_name"].lower() == repository.lower()
producer = args.receipts / "source-build"
artifacts = json.loads(subprocess.check_output(["gh", "api", f'repos/{repository}/actions/runs/{int(acceptance["source_build_run"])}/artifacts?per_page=100']))
available = {artifact["name"] for artifact in artifacts["artifacts"] if not artifact["expired"]}
artifact_name = "qualified-python-manifest" if "qualified-python-manifest" in available else "qualified-python-distributions"
assert artifact_name in available, "Qualified producer artifact is unavailable"
subprocess.run(["gh", "run", "download", str(int(acceptance["source_build_run"])),
                "--name", artifact_name, "--dir", str(producer)], check=True)
producer_manifest = json.loads((producer / "python-distributions.json").read_text())
assert producer_manifest == manifest, "Promoted distributions differ from the qualified source-build manifest"
qualification_inputs = args.receipts / "qualification-inputs"
subprocess.run(["gh", "run", "download", str(int(acceptance["source_build_run"])),
                "--pattern", "qualification-*", "--pattern", "candidate-native-manifest-*",
                "--dir", str(qualification_inputs)], check=True)
qualified = verify_qualification(qualification_inputs, manifest, current)
(args.receipts / "qualification-verification.json").write_text(json.dumps(qualified, indent=2) + "\n")
assert set(acceptance["gates"]["python_15_cells"]["native_image_sha256"]) == set(qualified["python_images"].values())
for gate in ["crypto_interoperability", "capi_adbc", "go", "swift"]:
    assert set(acceptance["gates"][gate]["native_image_sha256"]) == set(qualified["native_images"].values()), "Incomplete image coverage: " + gate
assert len(manifest["distributions"]) == 16
assert len([r for r in manifest["distributions"] if r["filename"].endswith(".whl")]) == 15
checksums = {}
for line in (args.receipts / "SHA256SUMS").read_text().splitlines():
    digest, name = line.split(maxsplit=1)
    name = name.lstrip("*")
    assert name not in checksums and re.fullmatch(r"[0-9a-f]{64}", digest)
    checksums[name] = digest
for metadata in ["launch-acceptance.json", "python-distributions.json", "apple-distribution.json"]:
    assert hashlib.sha256((args.receipts / metadata).read_bytes()).hexdigest() == checksums[metadata]
assets = {asset["name"] for asset in release["assets"]}
archives = args.receipts / "native-archives"
archives.mkdir()

def download_archive(name, expected):
    assert Path(name).name == name and name in assets and checksums[name] == expected
    subprocess.run(["gh", "release", "download", args.tag, "--dir", str(archives), "--pattern", name], check=True)
    path = archives / name
    assert sha(path) == expected
    return path

native_archive_verification = {}
for platform in qualified["native_platforms"]:
    native_producer = json.loads((qualification_inputs / ("candidate-native-manifest-" + platform) / "build-provenance.json").read_text())
    sdk = native_producer["sdk_archive"]
    assert sdk["filename"] == f"roastedduck-{version}-{platform}.tar.gz"
    archive = download_archive(sdk["filename"], sdk["sha256"])
    native_archive_verification[platform] = verify_archive(archive, native_producer)
(args.receipts / "native-archive-verification.json").write_text(json.dumps(native_archive_verification, indent=2) + "\n")
apple_distribution = json.loads((args.receipts / "apple-distribution.json").read_text())
assert apple_distribution["version"] == version and apple_distribution["filename"] == f"roastedduck-{version}-apple-arm64.tar.gz"
apple_archive = download_archive(apple_distribution["filename"], apple_distribution["sha256"])
apple_root = archives / "extracted-apple"
apple_root.mkdir()
with tarfile.open(apple_archive) as source:
    assert all(Path(member.name).parts[0] == "apple" for member in source.getmembers())
    source.extractall(apple_root, filter="data")
apple_source = apple_distribution["engine_source_identity_commit"]
assert re.fullmatch(r"[0-9a-f]{40}", apple_source)
subprocess.run(["git", "fetch", "--depth=1", "origin", apple_source], check=True)
apple_verified = verify_apple(apple_root / "apple", current, archives / "verification-tmp")
assert apple_verified["native_images"] == apple_distribution["native_images"]
(args.receipts / "apple-archive-verification.json").write_text(json.dumps(apple_verified, indent=2) + "\n")
expected_images = set(qualified["python_images"].values()) | set(qualified["native_images"].values()) | set(apple_verified["native_images"].values())
assert set(acceptance["promoted_native_image_sha256"]) == expected_images
assert set(acceptance["gates"]["final_artifact_integrity"]["native_image_sha256"]) == expected_images
assert set(acceptance["gates"]["apple_simulator"]["native_image_sha256"]) == {apple_verified["native_images"]["ios-simulator-arm64"]}
try:
    with urllib.request.urlopen(f"https://pypi.org/pypi/roastedduck/{version}/json", timeout=30) as response:
        existing = json.load(response)
except urllib.error.HTTPError as error:
    if error.code != 404:
        raise
    existing = {"urls": []}
expected_files = {record["filename"]: record["sha256"] for record in manifest["distributions"]}
for file in existing["urls"]:
    assert file["filename"] in expected_files, "Existing PyPI version contains an unapproved file"
    assert file["digests"]["sha256"] == expected_files[file["filename"]], "Existing immutable PyPI bytes differ"
for record in manifest["distributions"]:
    name = record["filename"]
    assert Path(name).name == name and name in assets
    assert record["sha256"] == checksums[name]
    subprocess.run(["gh", "release", "download", args.tag, "--dir", str(args.out), "--pattern", name], check=True)
    with (args.out / name).open("rb") as stream:
        assert hashlib.file_digest(stream, "sha256").hexdigest() == record["sha256"], name
subprocess.run(["python", "-m", "twine", "check", "--strict", *map(str, sorted(args.out.iterdir()))], check=True)
print("Verified 16 accepted distributions for PyPI", version)
