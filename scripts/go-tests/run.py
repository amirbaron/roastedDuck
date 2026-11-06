"""Execute an already signed Go upstream test binary and preserve every verdict."""

import argparse
from collections import Counter
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

from build import digest, linux_dependencies, verify_native


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", type=Path, required=True)
    parser.add_argument("--go", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--run", help="Go test selection; diagnostic subset, not full-suite acceptance")
    parser.add_argument("--signed-executable-root", type=Path,
                        help="Linux signed manifest directory containing the test executable")
    parser.add_argument("--certificate-sha256", help="trusted Linux signing certificate fingerprint")
    args = parser.parse_args()
    build, go, out = [path.resolve() for path in [args.build, args.go, args.out]]
    if out.exists():
        parser.error("preserve evidence: output must be new")
    report = json.loads((build / "summary.json").read_text())
    executable, library = Path(report["executable"]), Path(report["library"])
    if sys.platform.startswith("linux") and not (args.signed_executable_root and args.certificate_sha256):
        parser.error("Linux requires --signed-executable-root and --certificate-sha256 before executing tests")
    out.mkdir(parents=True)
    (out / "tmp").mkdir()
    for path, expected in [(executable, report["executable_sha256"]), (library, report["library_sha256"])]:
        assert digest(path) == expected
        signed_root = args.signed_executable_root if path == executable else report.get("signed_library_root")
        verify_native(path, signed_root, args.certificate_sha256, out / "tmp")
    source = out / "source"
    shutil.copytree(Path(report["source_copy"]), source)
    for name in ["telemetry"]:
        (out / name).mkdir()
    env = dict(os.environ, TMPDIR=str(out / "tmp"), GOTMPDIR=str(out / "tmp"),
               GOTELEMETRYDIR=str(out / "telemetry"))
    if sys.platform == "darwin":
        env["DYLD_PRINT_LIBRARIES"] = "1"
    else:
        env.pop("LD_LIBRARY_PATH", None)
        env.pop("LD_DEBUG_OUTPUT", None)
        env["LD_DEBUG"] = "libs"
        (out / "loader-before.log").write_text(linux_dependencies(executable, library, env))
    listed = subprocess.run([str(executable), "-test.list", "."], env=env, cwd=source,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30)
    (out / "list.stdout").write_text(listed.stdout)
    (out / "list.stderr").write_text(listed.stderr)
    assert listed.returncode == 0
    tests = [line for line in listed.stdout.splitlines() if line.startswith("Test")]
    assert tests and len(tests) == len(set(tests))
    selected = [test for test in tests if not args.run or re.search(args.run, test)]
    assert selected, "selection matches no listed top-level tests"
    command = [str(executable), "-test.v=test2json", "-test.parallel=1", "-test.timeout=20m"]
    if args.run:
        command.append("-test.run=" + args.run)
    priority = os.getpriority(os.PRIO_PROCESS, 0)
    if priority < 10:
        os.setpriority(os.PRIO_PROCESS, 0, 10)
    inherited_priority = os.getpriority(os.PRIO_PROCESS, 0)
    assert inherited_priority >= 10
    initial = {"build": str(build), "command": command, "inherited_nice": inherited_priority, "listed_tests": tests,
               "selected_top_level": selected, "selection": args.run,
               "executable_sha256": digest(executable), "library_sha256": digest(library)}
    (out / "manifest.json").write_text(json.dumps(initial, indent=2) + "\n")
    with (out / "run.stdout").open("x") as stdout, (out / "run.stderr").open("x") as stderr:
        # Launch directly: Apple's protected /usr/bin/nice strips DYLD_* before
        # exec. Inherit the parent's low priority without a Python fork callback,
        # preserving the loader evidence.
        result = subprocess.run(command, cwd=source, env=env, stdout=stdout, stderr=stderr,
                                timeout=1250)
    (out / "terminal.json").write_text(json.dumps({"exit_code": result.returncode}) + "\n")
    # Account for Go's explicit test markers without compiling/running a parser.
    events = []
    for line in (out / "run.stdout").read_text().splitlines():
        line = line.lstrip("\x16 \t")
        started = re.match(r"^=== RUN\s+(.+)$", line)
        terminal = re.match(r"^--- (PASS|FAIL|SKIP): (\S+) \(", line)
        if started:
            events.append({"Action": "run", "Test": started[1]})
        elif terminal:
            events.append({"Action": terminal[1].lower(), "Test": terminal[2]})
    with (out / "events.jsonl").open("x") as output:
        for event in events:
            output.write(json.dumps(event) + "\n")
    verdicts = {event["Test"]: event["Action"] for event in events
                if "Test" in event and event["Action"] in ["pass", "fail", "skip"]}
    started = {event["Test"] for event in events if event["Action"] == "run"}
    counts = dict(Counter(verdicts.values()))
    if result.returncode == 0:
        assert not (started - verdicts.keys()), "unfinished test cases"
        assert not (set(selected) - started), "unstarted selected test cases"
        assert not counts.get("fail", 0), "passing process reported failed test cases"
    stderr_lines = (out / "run.stderr").read_text().splitlines()
    if sys.platform == "darwin":
        loaded = [line for line in stderr_lines if "libduckdb.dylib" in line]
        assert loaded and all(str(library) in line for line in loaded), loaded
    else:
        loaded = [line for line in stderr_lines if re.search(r"calling (?:init|fini):\s+.*libduckdb\.so(?:\s|$)", line)]
        images = [re.search(r"calling (?:init|fini):\s+(.+?libduckdb\.so)\s*$", line)[1] for line in loaded]
        assert images and all(Path(image).resolve() == library.resolve() for image in images), loaded
    for path, expected in [(executable, report["executable_sha256"]), (library, report["library_sha256"])]:
        assert digest(path) == expected
        signed_root = args.signed_executable_root if path == executable else report.get("signed_library_root")
        verify_native(path, signed_root, args.certificate_sha256, out / "tmp")
    for name, expected in report["input_hashes"].items():
        assert digest(source / name) == expected, name
    summary = {**initial, "exit_code": result.returncode, "counts_including_subtests": counts,
               "verdicts": verdicts, "unfinished": sorted(started - verdicts.keys()),
               "unstarted_top_level": sorted(set(selected) - started), "runtime_engine_load_lines": loaded,
               "all_source_and_binary_hashes_unchanged": True,
               "scope": "Selected diagnostic subset" if args.run else
                   "Pinned dynamic Go root suite including Arrow; platform " + json.dumps(report.get("platform", {"goos": "darwin", "goarch": "arm64"}))}
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({k: summary[k] for k in ["exit_code", "counts_including_subtests", "unfinished", "unstarted_top_level"]}))
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
