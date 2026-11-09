"""Execute a signed Swift suite and verify its native image and full case coverage."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess

from build import verify_inputs


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", required=True, type=Path)
    parser.add_argument("--storage-probe", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    build, output, probe = args.build.resolve(), args.out.resolve(), args.storage_probe.resolve()
    if output.exists():
        parser.error("preserve prior evidence; --out must be new")
    processes = subprocess.check_output(["ps", "-axo", "pid,command"]).decode("utf-8", "replace")
    if any(re.search(r"/slt run\s", line) for line in processes.splitlines()):
        parser.error("finish the live SQL corpus before starting client tests")
    report = json.loads((build / "summary.json").read_text())
    assert report["build_exit_code"] == 0
    preparation = report["preparation"]
    assert len(preparation.get("additional_test_files", [])) == 1, "identity test is required"
    package = Path(report["package"])
    pin = json.loads((workspace / "swift/upstream.json").read_text())
    library = verify_inputs(package, preparation, pin)
    for item in report["native_artifacts"]:
        assert digest(Path(item["path"])) == item["sha256"]
        subprocess.run(["codesign", "--verify", "--strict", item["path"]], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(probe)], check=True)
    if report.get("target_architecture"):
        architectures = subprocess.check_output(["lipo", "-archs", str(probe)], text=True).split()
        assert architectures == [report["target_architecture"]], "storage probe must match the suite target"
    output.mkdir(parents=True)
    for name in ["tmp", "foundation-home", "fixtures"]:
        (output / name).mkdir()
    env = dict(os.environ, TMPDIR=str(output / "tmp") + "/",
               CFFIXED_USER_HOME=str(output / "foundation-home"),
               ROASTEDDUCK_SWIFT_TEST_DIRECTORY=str(output / "fixtures"),
               ROASTEDDUCK_SWIFT_EXPECTED_LIBRARY=str(library),
               ROASTEDDUCK_SWIFT_IDENTITY_REPORT=str(output / "identity.json"),
               CLANG_MODULE_CACHE_PATH=str(build / "module-cache"),
               SWIFTPM_MODULECACHE_OVERRIDE=str(build / "module-cache"))
    with (output / "storage-probe.json").open("x") as log:
        storage = subprocess.run(["nice", "-n", "10", str(probe), str(output), "--dedicated-fixtures"],
                                 env=env, stdout=log, stderr=subprocess.PIPE)
    assert storage.returncode == 0, storage.stderr.decode("utf-8", "replace")
    env["DYLD_PRINT_LIBRARIES"] = "1"
    command = ["nice", "-n", "10", "swift", "test", "--package-path", str(package),
               "--scratch-path", str(build / "build"), "--cache-path", str(build / "cache"),
               "--config-path", str(build / "config"), "--security-path", str(build / "security"),
               "--jobs", "1", "--skip-build", "--no-parallel", "--xunit-output", str(output / "junit.xml")]
    if report.get("target_triple"):
        command += ["--triple", report["target_triple"]]
    runner = "SwiftPM"
    if report.get("target_architecture") == "x86_64" and platform.machine() == "arm64":
        # This Xcode's SwiftPM helper can be arm64-only even though its compiler
        # cross-builds Intel bundles. Execute the complete XCTest bundle through
        # Apple's universal runner instead; no test filter or assertion changes.
        bundles = sorted((build / "build").rglob("*.xctest"))
        assert len(bundles) == 1, "one complete XCTest bundle is required"
        subprocess.run(["codesign", "--verify", "--strict", str(bundles[0])], check=True)
        xctest = subprocess.check_output(["xcrun", "--find", "xctest"], text=True).strip()
        architectures = subprocess.check_output(["lipo", "-archs", xctest], text=True).split()
        assert "x86_64" in architectures, "Intel XCTest runner is unavailable"
        command = ["arch", "-x86_64", xctest, str(bundles[0])]
        runner = "Complete XCTest bundle under Rosetta"
    (output / "manifest.json").write_text(json.dumps({"command": command, "build": str(build),
        "library": str(library), "library_sha256": preparation["library_sha256"],
        "target_triple": report.get("target_triple"),
        "runner": runner,
        "storage_probe_verified": True}, indent=2) + "\n")
    with (output / "run.log").open("x") as log:
        result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
    (output / "process-terminal.json").write_text(json.dumps({"exit_code": result.returncode}, indent=2) + "\n")
    text = (output / "run.log").read_text()
    started = re.findall(r"Test Case '-\[([^\]]+)\]' started\.", text)
    passed = re.findall(r"Test Case '-\[([^\]]+)\]' passed", text)
    failed = re.findall(r"Test Case '-\[([^\]]+)\]' failed", text)
    identity_cases = [name for name in started if ".LibraryIdentityTests " in name]
    upstream = [name for name in started if name not in identity_cases]
    identity = json.loads((output / "identity.json").read_text()) if (output / "identity.json").exists() else None
    library_valid = bool(identity and Path(identity["function_image"]).resolve() == library
                         and identity["engine_images"] == [str(library)])
    verify_inputs(package, preparation, pin)
    for item in report["native_artifacts"]:
        assert digest(Path(item["path"])) == item["sha256"]
        subprocess.run(["codesign", "--verify", "--strict", item["path"]], check=True)
    summary = {"ended_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
               "exit_code": result.returncode, "upstream_cases": len(upstream),
               "target_triple": report.get("target_triple"),
               "runner": runner,
               "identity_cases": len(identity_cases), "unique_started": len(set(started)),
               "passed": len(passed), "failed": failed, "case_ids": sorted(started),
               "runtime_library_verified": library_valid, "identity": identity,
               "source_library_artifact_hashes_unchanged": True,
               "scope": "132upstream XCTestcases plus1separate identity case; not portable distribution or all-client acceptance"}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({k: v for k, v in summary.items() if k != "case_ids"}))
    assert result.returncode == 0 and len(upstream) == 132 and len(identity_cases) == 1
    assert len(started) == len(set(started)) == len(passed) == 133 and set(started) == set(passed) and not failed
    assert library_valid, "in-process selected image proof missing or wrong"


if __name__ == "__main__":
    main()
