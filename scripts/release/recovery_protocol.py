"""Complete the two jobs GitHub never created after a recorded internal error."""
import json
import subprocess
from pathlib import Path

from observer_protocol import ROOT, REQUIRED_JOBS, api, verification_only_changes

RECOVERED_JOBS = {"qualify (ubuntu-24.04, linux-amd64)", "qualify (macos-26, macos-arm64)"}
RECOVERED_ARTIFACTS = {"qualification-linux-amd64", "qualification-macos-arm64",
                       "candidate-swift-linux-amd64", "candidate-swift-manifest-linux-amd64"}
INPUT_ARTIFACTS = {"recovery-input-linux-amd64", "recovery-input-macos-arm64"}
WORKFLOW = ".github/workflows/recover-qualification.yml"


def recovery_config():
    observer = json.loads((ROOT / "scripts/release/observer-inputs.json").read_text())
    pin = observer.get("qualification_recovery")
    if not pin:
        return None, None
    config = json.loads((ROOT / "scripts/release/recovery-inputs.json").read_text())
    assert all(config[k] == observer[k] for k in ["repository", "producer_run", "candidate_source_commit"])
    assert set(config["artifacts"]) == {"linux-amd64", "macos-arm64"}
    return pin, config


def validate_recovery(run, jobs, recovered_run, recovered_jobs, config, pin):
    """A recovery cannot hide a failed, missing, or changed native build/test."""
    repo, commit = config["repository"], config["candidate_source_commit"]
    assert run["id"] == config["producer_run"] and run["head_sha"] == commit
    assert run["head_repository"]["full_name"] == repo
    assert run["path"].split("@", 1)[0] == ".github/workflows/build-candidate.yml"
    assert run["status"] == "completed" and run["conclusion"] == "failure" and run["run_attempt"] == 1
    original = {j["name"]: j for j in jobs}
    assert len(original) == len(jobs) and set(original) == (REQUIRED_JOBS - RECOVERED_JOBS) | {"assemble"}
    assert original["assemble"]["conclusion"] == "skipped" and original["assemble"]["status"] == "completed"
    for name in REQUIRED_JOBS - RECOVERED_JOBS:
        assert original[name]["status"] == "completed" and original[name]["conclusion"] == "success", name
    assert recovered_run["id"] == pin["run"] and recovered_run["head_sha"] == pin["commit"]
    assert recovered_run["head_repository"]["full_name"] == repo
    assert recovered_run["path"].split("@", 1)[0] == WORKFLOW
    assert recovered_run["status"] == "completed" and recovered_run["conclusion"] == "success"
    recovered = {j["name"]: j for j in recovered_jobs}
    assert len(recovered) == len(recovered_jobs) and set(recovered) == RECOVERED_JOBS
    for name, job in recovered.items():
        assert job["status"] == "completed" and job["conclusion"] == "success", name
    return {"producer_run": run["id"], "producer_attempt": run["run_attempt"],
            "candidate_source_commit": commit, "producer_run_conclusion": "failure",
            "required_producer_jobs": [{"id": original[n]["id"], "name": n, "conclusion": "success"}
                                       for n in sorted(REQUIRED_JOBS - RECOVERED_JOBS)],
            "original_aggregate_diagnosis": {"job_id": original["assemble"]["id"], "conclusion": "skipped",
                "cause": config["provider_error"], "missing_jobs": sorted(RECOVERED_JOBS)},
            "qualification_recovery_run": recovered_run["id"],
            "qualification_recovery_commit": recovered_run["head_sha"],
            "qualification_recovery_attempt": recovered_run["run_attempt"],
            "qualification_recovery_jobs": [{"id": recovered[n]["id"], "name": n, "conclusion": "success"}
                                             for n in sorted(RECOVERED_JOBS)]}


def verify_workflow(candidate, recovery_commit):
    """The recovery runs the original job verbatim after authenticated download."""
    def read(commit, path):
        return subprocess.check_output(["git", "show", commit + ":" + path], cwd=ROOT, text=True)
    original = read(candidate, ".github/workflows/build-candidate.yml")
    actual = read(recovery_commit, WORKFLOW)
    original_steps = original.split("      - name: Run the compiled security and persistence tests natively\n", 1)[1].split("  assemble:\n", 1)[0]
    recovery_steps = actual.split("      - name: Run the compiled security and persistence tests natively\n", 1)[1].split("      - name: Retain immutable input identity\n", 1)[0]
    assert original_steps == recovery_steps, "Recovery changed runtime qualification steps"
    # The complete reviewed workflow and fetch helper are immutable after the
    # recovery commit; the native/source checkout is explicitly the producer.
    assert "ref: " + candidate + "\n" in actual
    subprocess.run(["git", "diff", "--exit-code", recovery_commit, "HEAD", "--", WORKFLOW,
                    "scripts/release/fetch_recovery_candidate.py", "scripts/release/recovery-inputs.json"],
                   cwd=ROOT, check=True)
    return verification_only_changes(candidate, recovery_commit)


def recovery_evidence(run, jobs, output):
    pin, config = recovery_config()
    assert pin and config
    repo = config["repository"]
    recovered = api(f'repos/{repo}/actions/runs/{pin["run"]}')
    recovered_jobs = api(f'repos/{repo}/actions/runs/{pin["run"]}/jobs?per_page=100')["jobs"]
    for name, data in [("recovery-run.json", recovered), ("recovery-jobs.json", recovered_jobs), ("recovery-inputs.json", config)]:
        (output / name).write_text(json.dumps(data, indent=2) + "\n")
    result = validate_recovery(run, jobs, recovered, recovered_jobs, config, pin)
    subprocess.run(["git", "fetch", "--depth=1", "origin", pin["commit"]], cwd=ROOT, check=True)
    result["recovery_verification_only_changed_paths"] = verify_workflow(config["candidate_source_commit"], pin["commit"])
    return result


def verify_input_receipts(incoming, evidence, producer_records):
    _, config = recovery_config()
    records = {r["name"]: r for r in producer_records}
    for platform, pin in config["artifacts"].items():
        receipt = json.loads((incoming / ("recovery-input-" + platform) / "recovery-input.json").read_text())
        assert receipt["passed"] is True and receipt["distributions_rebuilt"] is False
        assert receipt["source_commit"] == evidence["candidate_source_commit"]
        assert receipt["producer_run"] == evidence["producer_run"]
        assert receipt["recovery_run"] == evidence["qualification_recovery_run"]
        assert receipt["verification_tools_commit"] == evidence["qualification_recovery_commit"]
        assert receipt["platform"] == platform and receipt["artifact"] == pin == records[pin["name"]]
