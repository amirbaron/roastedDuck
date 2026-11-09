#!/usr/bin/env python3
"""Audit the actual thirty installed environments, without inferred resolution."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


def canonical(name):
    return re.sub(r"[-_.]+", "-", name).lower()


def collect(inputs):
    roots = [p for p in inputs.iterdir() if (p / "qualification/summary.json").is_file()]
    platforms = {p.name.removeprefix("qualification-").removeprefix("published-python-") for p in roots}
    assert len(roots) == 3 and platforms == {"linux-amd64", "linux-arm64", "macos-arm64"}
    inventories = {}
    for root in roots:
        summary = json.loads((root / "qualification/summary.json").read_text())
        assert len(summary["cells"]) == 5 and all(c["passed"] for c in summary["cells"])
        assert {c["abi"] for c in summary["cells"]} == {"cp310", "cp311", "cp312", "cp313", "cp314"}
        for cell in summary["cells"]:
            environments = {}
            for engine in ["cpp", "rust"]:
                directory = root / "qualification" / cell["abi"] / engine
                installed = json.loads((directory / "pip-inspect.json").read_text())["installed"]
                packages = {canonical(p["metadata"]["name"]): p["metadata"]["version"] for p in installed}
                assert packages and len(packages) == len(installed)
                path = (directory / "requirements.txt").resolve()
                entries = [line.split("==") for line in path.read_text().splitlines()]
                assert all(len(parts) == 2 for parts in entries)
                recorded = {canonical(name): version for name, version in entries}
                assert len(recorded) == len(entries) and recorded == packages, "Incomplete installed-package inventory: " + str(path)
                selected = "duckdb" if engine == "cpp" else "roastedduck"
                assert selected in packages and ({"duckdb", "roastedduck"} - {selected}).isdisjoint(packages)
                environments[engine] = {k: v for k, v in packages.items() if k != selected}
                inventories[path] = packages
            assert environments["cpp"] == environments["rust"], "Paired dependency environments differ"
    assert len(inventories) == 30
    return inventories


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--inputs", type=Path, required=True)
    p.add_argument("--cargo-lock", type=Path, required=True)
    p.add_argument("--scanner", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    inventories = collect(a.inputs)
    lock = a.cargo_lock.resolve()
    assert lock.name == "Cargo.lock"
    a.out.parent.mkdir(parents=True, exist_ok=True)
    command = [str(a.scanner.resolve()), "scan", "source", "--lockfile", str(lock),
               "--no-call-analysis", "rust", "--no-resolve", "--all-vulns", "--all-packages",
               "--format", "json", "--output-file", str(a.out)]
    for path in sorted(inventories):
        command += ["--lockfile", str(path)]
    subprocess.run(command, check=True)
    result = json.loads(a.out.read_text())
    sources = {Path(r["source"]["path"]).resolve(): r for r in result["results"]}
    assert len(sources) == len(result["results"]) and set(sources) == set(inventories) | {lock}
    assert all(not package.get("vulnerabilities") for source in sources.values() for package in source["packages"])
    for path, expected in inventories.items():
        actual = {canonical(p["package"]["name"]): p["package"]["version"] for p in sources[path]["packages"]}
        assert actual == expected, "Scanner omitted installed packages: " + str(path)
    proof = {"passed": True, "resolved_environment_count": 30, "source_count": len(sources),
             "cargo_lock_sha256": hashlib.sha256(lock.read_bytes()).hexdigest(),
             "inventory_sha256": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in inventories},
             "package_records": sum(len(source["packages"]) for source in sources.values())}
    a.out.with_suffix(".inputs.json").write_text(json.dumps(proof, indent=2) + "\n")
    print("Audited all thirty complete installed environments and the specified Rust lockfile")


if __name__ == "__main__":
    main()
