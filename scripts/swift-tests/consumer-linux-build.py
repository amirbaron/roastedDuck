"""Relocate a Linux Swift package and build an independent consumer without running it."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

from prepare import digest


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    sdk, out = args.sdk.resolve(), args.out.resolve()
    if out.exists():
        parser.error("preserve evidence: --out must be new")
    report = json.loads((sdk / "summary.json").read_text())
    original = Path(report["package"])
    for name, expected in report["files"].items():
        assert digest(original / name) == expected, name
    out.mkdir(parents=True)
    relocated = out / "relocated/DuckDBRust"
    shutil.copytree(original, relocated)
    client = out / "client"
    (client / "Sources/Consumer").mkdir(parents=True)
    shutil.copy2(workspace / "scripts/swift-tests/Consumer.swift", client / "Sources/Consumer/main.swift")
    helper = client / "Sources/TestNativeImage"
    (helper / "include").mkdir(parents=True)
    shutil.copy2(workspace / "scripts/swift-tests/NativeImageProbe.c", helper / "NativeImageProbe.c")
    shutil.copy2(workspace / "scripts/swift-tests/NativeImageProbe.h", helper / "include/NativeImageProbe.h")
    (client / "Package.swift").write_text('''// swift-tools-version: 5.7
import PackageDescription
let package = Package(
    name: "Consumer",
    dependencies: [.package(path: "../relocated/DuckDBRust")],
    targets: [
        .target(name: "TestNativeImage", publicHeadersPath: "include", linkerSettings: [.linkedLibrary("dl")]),
        .executableTarget(name: "Consumer", dependencies: [
            .product(name: "DuckDB", package: "DuckDBRust"), "TestNativeImage"])
    ]
)
''')
    dirs = {name: out / name for name in ["build", "cache", "config", "security", "tmp", "module-cache"]}
    for directory in dirs.values():
        directory.mkdir()
    env = dict(os.environ, TMPDIR=str(dirs["tmp"]),
               SWIFTPM_MODULECACHE_OVERRIDE=str(dirs["module-cache"]),
               CLANG_MODULE_CACHE_PATH=str(dirs["module-cache"]))
    env.pop("LD_LIBRARY_PATH", None)
    command = ["nice", "-n", "10", "swift", "build", "--jobs", "1", "--package-path", str(client),
               "--scratch-path", str(dirs["build"]), "--cache-path", str(dirs["cache"]),
               "--config-path", str(dirs["config"]), "--security-path", str(dirs["security"])]
    (out / "manifest.json").write_text(json.dumps({"command": command,
        "consumer_source_sha256": digest(client / "Sources/Consumer/main.swift")}, indent=2) + "\n")
    with (out / "build.log").open("x") as log:
        built = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
    (out / "build-terminal.json").write_text(json.dumps({"exit_code": built.returncode}) + "\n")
    if built.returncode:
        raise SystemExit(built.returncode)
    binaries = [p for p in dirs["build"].rglob("Consumer")
                if p.is_file() and not p.is_symlink() and p.open("rb").read(4) == b"\x7fELF"]
    assert len(binaries) == 1, binaries
    executable = binaries[0]
    dynamic = subprocess.check_output(["readelf", "-d", str(executable)], text=True)
    needed = re.findall(r"\(NEEDED\).*?\[(.*?)\]", dynamic)
    assert [name for name in needed if "duckdb" in name.lower()] == ["libduckdb.so"], dynamic
    assert "$ORIGIN/lib" in dynamic
    loader = subprocess.check_output(["ldd", str(executable)], env=env, text=True)
    library = relocated / "Artifacts/lib/libduckdb.so"
    assert str(library) in loader, loader
    for name, expected in report["files"].items():
        assert digest(original / name) == expected, name
        assert digest(relocated / name) == expected, name
    result = {"binary": str(executable), "binary_sha256": digest(executable),
              "library": str(library), "library_sha256": digest(library),
              "dynamic_section": dynamic, "loader_output": loader,
              "original_package": str(original), "relocated_package": str(relocated),
              "package_hashes_unchanged": True, "build_exit_code": built.returncode,
              "execution_requires_detached_signature": True,
              "scope": "Independent Linux Swift consumer build; runtime and persistence verification pending"}
    (out / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ["dynamic_section", "loader_output"]}))


if __name__ == "__main__":
    main()
