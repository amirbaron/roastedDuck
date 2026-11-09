#!/usr/bin/env python3
"""Install and compare the actual five ABI wheels against pinned upstream controls.

Produces per-process identities, raw JUnit, skip/failure verdicts and installation
receipts. A failed reference test is retained and does not count as a pass.
"""
import argparse
from collections import Counter
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
VERSION = json.loads((ROOT / "release.json").read_text())["version"]


def run(command, log, env, cwd):
    with log.open("w") as output:
        result = subprocess.run(command, stdout=output, stderr=subprocess.STDOUT, env=env, cwd=cwd)
    return result.returncode


def verdicts(path):
    records = {}
    for case in ET.parse(path).iter("testcase"):
        key = case.attrib.get("classname", "") + "::" + case.attrib["name"]
        assert key not in records, key
        child = next((c for c in case if c.tag in {"failure", "error", "skipped"}), None)
        records[key] = {"verdict": "pass" if child is None else child.tag,
                        "type": "" if child is None else child.attrib.get("type", ""),
                        "reason": "" if child is None else child.attrib.get("message", "")}
    assert records, "No upstream tests executed"
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wheels", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, help="Bind published wheels and native images to the accepted release manifest")
    parser.add_argument("--versions", nargs="+", default=["3.10", "3.11", "3.12", "3.13", "3.14"])
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.wheels = args.wheels.resolve()
    args.source = args.source.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    manifest = json.loads(args.manifest.read_text()) if args.manifest else None
    version = manifest["version"] if manifest else VERSION
    if manifest:
        assert re.fullmatch(r"[0-9a-f]{40}", manifest["source_commit"])
        release = json.loads(subprocess.check_output(["git", "show", manifest["source_commit"] + ":release.json"], cwd=ROOT, text=True))
        assert release["version"] == version
    else:
        release = json.loads((ROOT / "release.json").read_text())
    (args.out / "release.json").write_text(json.dumps(release, indent=2) + "\n")
    accepted = {record["filename"]: record for record in manifest["distributions"]} if manifest else {}
    provenance = {
        "validation_tools_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "validation_requirements_sha256": hashlib.sha256((ROOT / "scripts/release/requirements-validation.txt").read_bytes()).hexdigest(),
        "corpus_directory": args.source.name,
        "reference_distribution": "duckdb==" + release["compatibility_version"],
        "accepted_manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest() if manifest else None,
        "candidate_source_commit": manifest["source_commit"] if manifest else None,
    }
    (args.out / "validation-provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    reports = []
    for python_version in args.versions:
        executable = shutil.which("python" + python_version)
        assert executable, f"Required native interpreter Python {python_version} is unavailable"
        abi = "cp" + python_version.replace(".", "")
        wheels = list(args.wheels.glob(f"roastedduck-{version}-{abi}-{abi}-*.whl"))
        assert len(wheels) == 1, wheels
        if manifest:
            assert hashlib.sha256(wheels[0].read_bytes()).hexdigest() == accepted[wheels[0].name]["sha256"]
        cell = args.out / abi
        cell.mkdir()
        results = {}
        for engine in ["cpp", "rust"]:
            output = cell / engine
            output.mkdir()
            env = dict(os.environ, TMPDIR=str(output / "tmp"), PYTHONDONTWRITEBYTECODE="1", TZ="UTC",
                       PIP_CONFIG_FILE=os.devnull, PIP_DISABLE_PIP_VERSION_CHECK="1")
            Path(env["TMPDIR"]).mkdir()
            env.pop("PYTHONPATH", None)
            env.pop("PIP_INDEX_URL", None)
            env.pop("PIP_EXTRA_INDEX_URL", None)
            for key in list(env):
                if key.startswith(("AWS_", "AZURE_", "OPENAI_", "ANTHROPIC_", "VERCEL_", "TWINE_", "PYPI_")) or key in {"GH_TOKEN", "GITHUB_TOKEN", "GOOGLE_APPLICATION_CREDENTIALS"}:
                    env.pop(key)
            venv = output / "venv"
            subprocess.run([executable, "-m", "venv", str(venv)], env=env, check=True)
            python = str(venv / "bin/python")
            requirements = [python, "-m", "pip", "install", "--only-binary=:all:", "-r", str(ROOT / "scripts/release/requirements-validation.txt")]
            if engine == "rust":
                requirements += ["--constraint", str(cell / "cpp/requirements.txt")]
            assert run(requirements, output / "dependencies.log", env, output) == 0
            package = str(wheels[0]) if engine == "rust" else "duckdb==" + release["compatibility_version"]
            install = [python, "-m", "pip", "install", "--no-deps", "--only-binary=:all:", "--report", str(output / "install.json"), package]
            assert run(install, output / "install.log", env, output) == 0
            assert run([python, "-m", "pip", "check"], output / "pip-check.log", env, output) == 0
            assert run([python, "-m", "pip", "freeze", "--all"], output / "pip-freeze.txt", env, output) == 0
            assert run([python, "-m", "pip", "inspect"], output / "pip-inspect.json", env, output) == 0
            installed = json.loads((output / "pip-inspect.json").read_text())["installed"]
            resolved = {item["metadata"]["name"]: item["metadata"]["version"] for item in installed}
            assert len(resolved) == len(installed)
            (output / "requirements.txt").write_text("".join(f"{name}=={version}\n" for name, version in sorted(resolved.items())))
            identity = subprocess.check_output([python, "-c", "import duckdb,_duckdb,hashlib,json;from pathlib import Path;p=Path(_duckdb.__file__);print(json.dumps({'rust':bool(getattr(_duckdb,'__rust_engine__',False)),'native':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}))"], cwd=output, env=env, text=True)
            identity = json.loads(identity)
            assert identity["rust"] == (engine == "rust")
            if engine == "rust" and manifest:
                assert identity["sha256"] == accepted[wheels[0].name]["native_sha256"]
            (output / "identity.json").write_text(json.dumps(identity, indent=2) + "\n")
            if engine == "rust":
                assert run([python, str(ROOT / "scripts/release/smoke_python.py"), "--release", str(args.out / "release.json")], output / "smoke.json", env, output) == 0
                assert run([python, str(ROOT / "scripts/release/smoke_encryption.py")], output / "encryption.json", env, output) == 0
            env.update(PYTHONPATH=str(ROOT / "scripts/release"), ROASTEDDUCK_TEST_ENGINE=engine,
                       ROASTEDDUCK_EXPECTED_NATIVE_SHA256=identity["sha256"],
                       ROASTEDDUCK_PYTEST_IDENTITY=str(output / "pytest-identity.json"),
                       ROASTEDDUCK_PYTEST_EVENTS=str(output / "pytest-events.jsonl"))
            command = [python, "-m", "pytest", "-q", "-p", "pytest_release_guard", "--continue-on-collection-errors", "--timeout=120",
                       "-o", "cache_dir=" + str(output / "pytest-cache"), "--basetemp=" + str(output / "pytest-tmp"),
                       "--junitxml=" + str(output / "junit.xml"), str(args.source / "tests/fast")]
            code = run(command, output / "pytest.log", env, output)
            raw = verdicts(output / "junit.xml")
            (output / "verdicts.json").write_text(json.dumps(raw, indent=2) + "\n")
            events = [json.loads(line) for line in (output / "pytest-events.jsonl").read_text().splitlines()]
            statuses = {}
            for event in events:
                if event["when"] == "call" or event["outcome"] != "passed":
                    key = event["nodeid"] + "::" + event["when"]
                    statuses[key] = {"outcome": event["outcome"], "wasxfail": event["wasxfail"]}
            results[engine] = {"exit_code": code, "counts": dict(Counter(v["verdict"] for v in raw.values())), "cases": raw,
                               "explicit_outcomes": statuses,
                               "xfails": sum(e["outcome"] == "skipped" and e["wasxfail"] is not None for e in events),
                               "xpasses": sum(e["outcome"] == "passed" and e["wasxfail"] is not None for e in events)}
        cpp, rust = results["cpp"]["cases"], results["rust"]["cases"]
        environments = {}
        for engine in ["cpp", "rust"]:
            installed = json.loads((cell / engine / "pip-inspect.json").read_text())["installed"]
            environments[engine] = {item["metadata"]["name"].lower().replace("_", "-"): item["metadata"]["version"]
                                    for item in installed if item["metadata"]["name"].lower() not in {"duckdb", "roastedduck"}}
        assert environments["cpp"] == environments["rust"], "Control and Rust validation environments differ"
        differences = {key: {"cpp": cpp.get(key), "rust": rust.get(key)} for key in sorted(cpp.keys() | rust.keys()) if cpp.get(key) != rust.get(key)}
        event_differences = results["cpp"]["explicit_outcomes"] != results["rust"]["explicit_outcomes"]
        record = {"python": python_version, "abi": abi, "wheel": wheels[0].name, "engines": results,
                  "differences": differences, "explicit_outcome_difference": event_differences,
                  "passed": not differences and not event_differences and all(r["exit_code"] == 0 for r in results.values())}
        (cell / "comparison.json").write_text(json.dumps(record, indent=2) + "\n")
        reports.append({k: v for k, v in record.items() if k != "engines"})
        (args.out / "summary.json").write_text(json.dumps({"version": version, "cells": reports}, indent=2) + "\n")
        print(abi, "PASS" if record["passed"] else "FAIL", flush=True)
    assert all(r["passed"] for r in reports), "Candidate qualification failed; see raw comparisons"


if __name__ == "__main__":
    main()
