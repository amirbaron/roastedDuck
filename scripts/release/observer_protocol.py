"""Strict producer/observer provenance; a failed runtime gate is never accepted."""
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parents[2]
REQUIRED_JOBS = {
    "security", "sdist",
    "build (ubuntu-24.04, ubuntu-24.04, linux-amd64, x86_64-unknown-linux-gnu, 2_28)",
    "build (ubuntu-24.04-arm, ubuntu-24.04-arm, linux-arm64, aarch64-unknown-linux-gnu, 2_28)",
    "build (macos-26-intel, macos-26, macos-arm64, aarch64-apple-darwin, off)",
    "qualify (ubuntu-24.04, linux-amd64)", "qualify (ubuntu-24.04-arm, linux-arm64)",
    "qualify (macos-26, macos-arm64)",
}
VERIFICATION_ONLY_PATHS = {
    ".github/workflows/observe-candidate.yml", ".github/workflows/pypi-publish.yml",
    "scripts/release/pip_report.py", "scripts/release/verify_qualification.py",
    "scripts/release/verify_pypi_install.py", "scripts/release/assemble_python_candidate.py",
    "scripts/release/prepare_pypi.py", "scripts/release/observe_candidate.py",
    "scripts/release/observer_protocol.py", "scripts/release/observer-inputs.json",
    "scripts/release/fetch_observer_inputs.py",
    "scripts/release/README.md", "scripts/release/tests/test_pip_report.py",
    "scripts/release/tests/test_observer_protocol.py",
    "scripts/release/tests/test_qualification_gate.py",
    ".github/workflows/recover-qualification.yml", "scripts/release/fetch_recovery_candidate.py",
    "scripts/release/recovery-inputs.json", "scripts/release/recovery_protocol.py",
    "scripts/release/tests/test_recovery_protocol.py",
}

def required_artifact_names():
    names = ["security-report", "candidate-sdist"]
    for platform in ["macos-arm64", "linux-arm64", "linux-amd64"]:
        names += ["candidate-" + platform, "candidate-native-manifest-" + platform, "qualification-" + platform]
    for platform in ["linux-arm64", "linux-amd64"]:
        names += ["candidate-swift-" + platform, "candidate-swift-manifest-" + platform]
    return names

def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

def api(path):
    return json.loads(subprocess.check_output(["gh", "api", path], text=True))

def verification_only_changes(candidate, observer):
    assert re.fullmatch(r"[0-9a-f]{40}", candidate) and re.fullmatch(r"[0-9a-f]{40}", observer)
    changed = subprocess.check_output(["git", "diff", "--name-only", candidate, observer], cwd=ROOT, text=True).splitlines()
    assert changed and set(changed) <= VERIFICATION_ONLY_PATHS, "Product/build-input changes require a new producer: " + repr(changed)
    return changed

def validate_producer(run, jobs, repository, commit, aggregate_log=None):
    assert run["head_sha"] == commit and run["head_repository"]["full_name"] == repository
    assert run["path"].split("@", 1)[0] == ".github/workflows/build-candidate.yml"
    assert run["status"] == "completed"
    by_name = {job["name"]: job for job in jobs}
    assert len(by_name) == len(jobs) and set(by_name) == REQUIRED_JOBS | {"assemble"}
    for name in REQUIRED_JOBS:
        job = by_name[name]
        assert job["status"] == "completed" and job["conclusion"] == "success", "Unqualified producer gate: " + name
    aggregate = by_name["assemble"]
    assert aggregate["status"] == "completed"
    if aggregate["conclusion"] == "success":
        assert run["conclusion"] == "success"
        diagnosis = None
    else:
        assert run["conclusion"] == aggregate["conclusion"] == "failure"
        failures = [step for step in aggregate["steps"] if step["conclusion"] == "failure"]
        assert len(failures) == 1 and "scripts/release/verify_qualification.py" in failures[0]["name"]
        assert aggregate_log and "KeyError: 'hashes'" in aggregate_log and "verify_qualification.py" in aggregate_log
        diagnosis = {"job_id": aggregate["id"], "step": failures[0]["name"],
                     "error": "KeyError: 'hashes'", "cause": "Legacy pip SHA-256 report schema; re-evaluated without changing any distribution bytes",
                     "log_sha256": hashlib.sha256(aggregate_log.encode()).hexdigest()}
    return {"producer_run": run["id"], "producer_attempt": run["run_attempt"],
            "candidate_source_commit": commit, "producer_run_conclusion": run["conclusion"],
            "required_producer_jobs": [{"id": by_name[name]["id"], "name": name, "conclusion": "success"} for name in sorted(REQUIRED_JOBS)],
            "original_aggregate_diagnosis": diagnosis}

def download_artifact(repository, record, run_id, commit, destination, temporary):
    assert record["workflow_run"]["id"] == run_id and record["workflow_run"]["head_sha"] == commit
    assert not record["expired"] and re.fullmatch(r"sha256:[0-9a-f]{64}", record["digest"])
    destination.mkdir(parents=True, exist_ok=False)
    archive = temporary / (str(record["id"]) + ".zip")
    with archive.open("wb") as output:
        subprocess.run(["gh", "api", f'repos/{repository}/actions/artifacts/{int(record["id"])}/zip'], stdout=output, check=True)
    assert sha(archive) == record["digest"].split(":", 1)[1]
    seen = set()
    with zipfile.ZipFile(archive) as source:
        for entry in source.infolist():
            path = PurePosixPath(entry.filename)
            assert not path.is_absolute() and ".." not in path.parts and str(path) not in seen
            assert not stat.S_ISLNK(entry.external_attr >> 16)
            seen.add(str(path))
        source.extractall(destination)
    archive.unlink()  # Only this completed, digest-verified temporary download.
    return {key: record[key] for key in ["id", "name", "digest", "size_in_bytes"]}

def source_run_evidence(repository, run_id, commit, output):
    run = api(f"repos/{repository}/actions/runs/{run_id}")
    jobs = api(f"repos/{repository}/actions/runs/{run_id}/jobs?per_page=100")["jobs"]
    (output / "producer-run.json").write_text(json.dumps(run, indent=2) + "\n")
    (output / "producer-jobs.json").write_text(json.dumps(jobs, indent=2) + "\n")
    aggregate = next((job for job in jobs if job["name"] == "assemble"), None)
    log = None
    if aggregate and aggregate["conclusion"] == "failure":
        log = subprocess.check_output(["gh", "api", f'repos/{repository}/actions/jobs/{aggregate["id"]}/logs'], text=True)
        (output / "original-aggregate-failure.log").write_text(log)
    from recovery_protocol import recovery_config, recovery_evidence
    recovery_pin, _ = recovery_config()
    if recovery_pin:
        evidence = recovery_evidence(run, jobs, output)
        assert evidence["candidate_source_commit"] == commit and evidence["producer_run"] == run_id
    else:
        evidence = validate_producer(run, jobs, repository, commit, log)
    (output / "producer-run.json").write_text(json.dumps(run, indent=2) + "\n")
    (output / "producer-jobs.json").write_text(json.dumps(jobs, indent=2) + "\n")
    return evidence

def verify_observer_chain(repository, acceptance, candidate, receipts):
    """Recheck both GitHub runs and immutable artifact identities at publication."""
    observer_id = int(acceptance["observer_run"])
    observer_commit = acceptance["verification_tools_commit"]
    assert re.fullmatch(r"[0-9a-f]{40}", observer_commit)
    run = api(f"repos/{repository}/actions/runs/{observer_id}")
    assert run["status"] == "completed" and run["conclusion"] == "success"
    assert run["head_sha"] == observer_commit and run["head_repository"]["full_name"] == repository
    assert run["path"].split("@", 1)[0] == ".github/workflows/observe-candidate.yml"
    subprocess.run(["git", "fetch", "--depth=1", "origin", observer_commit], cwd=ROOT, check=True)
    changed = verification_only_changes(candidate, observer_commit)
    subprocess.run(["git", "diff", "--exit-code", observer_commit, "HEAD", "--",
                    "scripts/release/observer_protocol.py", "scripts/release/observe_candidate.py",
                    "scripts/release/pip_report.py", "scripts/release/verify_qualification.py",
                    "scripts/release/verify_apple_release.py", "scripts/release/archive_native_sdk.py",
                    "scripts/release/qualify_swift_package.py", "scripts/release/inspect_distribution.py"], cwd=ROOT, check=True)
    subprocess.run(["git", "diff", "--exit-code", observer_commit, "HEAD", "--",
                    "scripts/release/recovery_protocol.py", "scripts/release/recovery-inputs.json",
                    "scripts/release/observer-inputs.json", "scripts/release/fetch_recovery_candidate.py",
                    ".github/workflows/recover-qualification.yml"], cwd=ROOT, check=True)
    catalog = api(f"repos/{repository}/actions/runs/{observer_id}/artifacts?per_page=100")["artifacts"]
    matches = [item for item in catalog if item["name"] == "observed-manifests"]
    assert len(matches) == 1
    artifact = api(f'repos/{repository}/actions/artifacts/{int(matches[0]["id"])}')
    assert artifact["digest"] == matches[0]["digest"]
    temporary = receipts / "observer-download-tmp"
    temporary.mkdir()
    directory = receipts / "observer-manifests"
    download_artifact(repository, artifact, observer_id, observer_commit, directory, temporary)
    observed = json.loads((directory / "observer-verification.json").read_text())
    assert observed["passed"] is True and observed["distributions_rebuilt"] is False and observed["distribution_bytes_changed"] is False
    assert observed["observer_run"] == observer_id and observed["verification_tools_commit"] == observer_commit
    assert observed["verification_only_changed_paths"] == changed
    assert sha(directory / "observer-verification.json") == acceptance["observer_verification_sha256"]
    producer_id = int(acceptance["source_build_run"])
    source_output = receipts / "producer-rechecked"
    source_output.mkdir()
    actual = source_run_evidence(repository, producer_id, candidate, source_output)
    for key, value in actual.items():
        assert observed[key] == value, "Producer evidence differs: " + key
    assert acceptance["producer_run_conclusion"] == actual["producer_run_conclusion"]
    records = observed["producer_artifacts"]
    recovered_records = observed.get("recovery_artifacts", [])
    from recovery_protocol import RECOVERED_ARTIFACTS, INPUT_ARTIFACTS, verify_input_receipts
    expected_originals = set(required_artifact_names())
    if "qualification_recovery_run" in actual:
        expected_originals -= RECOVERED_ARTIFACTS
        assert len(recovered_records) == len(RECOVERED_ARTIFACTS | INPUT_ARTIFACTS)
        assert {r["name"] for r in recovered_records} == RECOVERED_ARTIFACTS | INPUT_ARTIFACTS
        recovery_catalog = api(f'repos/{repository}/actions/runs/{actual["qualification_recovery_run"]}/artifacts?per_page=100')["artifacts"]
        recovery_originals = {r["id"]: r for r in recovery_catalog}
        for record in recovered_records:
            original = recovery_originals[record["id"]]
            assert not original["expired"] and all(original[k] == record[k] for k in ["id", "name", "digest", "size_in_bytes"])
            assert original["workflow_run"]["id"] == actual["qualification_recovery_run"]
            assert original["workflow_run"]["head_sha"] == actual["qualification_recovery_commit"]
        verify_input_receipts(directory / "qualification-inputs", actual, records)
    else:
        assert not recovered_records
    assert len(records) == len(expected_originals) and {record["name"] for record in records} == expected_originals
    producer_catalog = api(f"repos/{repository}/actions/runs/{producer_id}/artifacts?per_page=100")["artifacts"]
    originals = {record["id"]: record for record in producer_catalog}
    for record in records:
        original = originals[record["id"]]
        assert not original["expired"] and all(original[key] == record[key] for key in ["id", "name", "digest", "size_in_bytes"])
        assert original["workflow_run"]["head_sha"] == candidate and original["workflow_run"]["id"] == producer_id
    assert sha(directory / "python-distributions.json") == observed["python_manifest_sha256"]
    return directory, observed
