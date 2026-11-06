"""Prepare a pinned Swift C API test package without building or bundling an engine."""

import argparse
import datetime
import difflib
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

from adapt import apply_source_adaptations


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path, help="frozen DuckDB source root")
    parser.add_argument("--library", required=True, type=Path, help="explicit libduckdb.dylib/.so")
    parser.add_argument("--engine", required=True, choices=["rust", "cpp"])
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--dedicated-test-fixtures", action="store_true")
    parser.add_argument("--identity-test", action="store_true")
    args = parser.parse_args()
    source, library, output = args.source.resolve(), args.library.resolve(), args.out.resolve()
    if output.exists():
        parser.error("preserve previous artifacts; --out must be new")
    if not library.is_file() or library.name not in ["libduckdb.dylib", "libduckdb.so"]:
        parser.error("--library must name an existing explicit libduckdb image")
    if sys.platform == "darwin":
        subprocess.run(["codesign", "--verify", "--strict", str(library)], check=True)
    pin_path = workspace / "swift/upstream.json"
    pin = json.loads(pin_path.read_text())
    client = source / "tools/swift/duckdb-swift"
    header = source / "src/include/duckdb.h"
    if digest(header) != pin["header_sha256"]:
        parser.error("C header does not match the frozen client pin")
    for item in pin["files"]:
        path = client / item["path"]
        if not path.is_file() or digest(path) != item["sha256"]:
            parser.error("source/test/license hash mismatch: " + item["path"])
    output.mkdir(parents=True)
    for item in pin["files"]:
        destination = output / item["path"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(client / item["path"], destination)
    include = output / "Sources/Cduckdb/include"
    include.mkdir(parents=True)
    shutil.copy2(header, include / "duckdb.h")
    (output / "Sources/Cduckdb/Cduckdb.c").write_text('#include "duckdb.h"\n')
    # SwiftPM can create libDuckDB.a for the wrapper itself; -lduckdb can
    # resolve that archive on case-insensitive volumes. Link the image directly.
    flags = ["-Xlinker", str(library)]
    if sys.platform == "darwin":
        flags += ["-Xlinker", "-rpath", "-Xlinker", str(library.parent),
                  "-Xlinker", "-headerpad_max_install_names"]
    elif sys.platform.startswith("linux"):
        flags += ["-Xlinker", "-rpath", "-Xlinker", str(library.parent)]
    swift_flags = ", ".join(json.dumps(flag, ensure_ascii=False) for flag in flags)
    manifest = '''// swift-tools-version: 5.7
import PackageDescription

let package = Package(
    name: "DuckDB",
    products: [.library(name: "DuckDB", targets: ["DuckDB"])],
    targets: [
        .target(name: "Cduckdb", publicHeadersPath: "include"),
        .target(name: "DuckDB", dependencies: ["Cduckdb"],
                linkerSettings: [.unsafeFlags([FLAGS])]),
        .testTarget(name: "DuckDBTests", dependencies: ["DuckDB"])
    ]
)
'''.replace("FLAGS", swift_flags)
    (output / "Package.swift").write_text(manifest)
    assert not list(output.rglob("*.cpp")) and not list(output.rglob("*.cxx"))
    for item in pin["files"]:
        assert digest(output / item["path"]) == item["sha256"]
    source_adaptations = apply_source_adaptations(output)
    adaptations = []
    if args.dedicated_test_fixtures:
        path = output / "Tests/DuckDBTests/DatabaseTests.swift"
        original = path.read_text()
        old = '''    let tmpDirURL = try FileManager.default.url(
      for: .itemReplacementDirectory,
      in: .userDomainMask,
      appropriateFor: FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0],
      create: true
    )'''
        new = '''    guard let directory = ProcessInfo.processInfo.environment["RUSTDB_SWIFT_TEST_DIRECTORY"] else {
      throw CocoaError(.fileNoSuchFile)
    }
    let tmpDirURL = URL(fileURLWithPath: directory, isDirectory: true)
      .appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: tmpDirURL, withIntermediateDirectories: true)'''
        assert original.count(old) == 1, "frozen path helper changed"
        patched = original.replace(old, new, 1)
        assert original.split("// MARK: - Temp Directory Helpers")[0] == patched.split("// MARK: - Temp Directory Helpers")[0]
        path.write_text(patched)
        (output / "fixture-path.diff").write_text("".join(difflib.unified_diff(
            original.splitlines(True), patched.splitlines(True),
            fromfile="upstream/DatabaseTests.swift", tofile="dedicated-storage/DatabaseTests.swift")))
        adaptations.append({"path": str(path.relative_to(output)),
                            "original_sha256": hashlib.sha256(original.encode()).hexdigest(),
                            "patched_sha256": digest(path),
                            "reason": "Dedicated test directory; all upstream assertions unchanged"})
    additional_tests = []
    test_infrastructure = []
    if args.identity_test:
        source_test = workspace / "scripts/swift-tests/LibraryIdentityTests.swift"
        destination = output / "Tests/DuckDBTests/LibraryIdentityTests.swift"
        shutil.copy2(source_test, destination)
        additional_tests.append({"path": str(destination.relative_to(output)),
                                 "sha256": digest(destination),
                                 "scope": "Additional native library identity test; not one of132upstream cases"})
        if sys.platform.startswith("linux"):
            constants = output / "Tests/DuckDBTests/FoundationPlatformConstants.swift"
            shutil.copy2(workspace / "scripts/swift-tests/FoundationPlatformConstants.swift", constants)
            test_infrastructure.append({"path": str(constants.relative_to(output)),
                                        "sha256": digest(constants)})
            helper = output / "Sources/TestNativeImage"
            (helper / "include").mkdir(parents=True)
            for name, destination in [("NativeImageProbe.c", helper / "NativeImageProbe.c"),
                                      ("NativeImageProbe.h", helper / "include/NativeImageProbe.h")]:
                shutil.copy2(workspace / "scripts/swift-tests" / name, destination)
                test_infrastructure.append({"path": str(destination.relative_to(output)),
                                            "sha256": digest(destination)})
            original_target = '.testTarget(name: "DuckDBTests", dependencies: ["DuckDB"])'
            assert manifest.count(original_target) == 1
            manifest = manifest.replace(original_target,
                '.target(name: "TestNativeImage", publicHeadersPath: "include", linkerSettings: [.linkedLibrary("dl")]),\n'
                '        .testTarget(name: "DuckDBTests", dependencies: ["DuckDB", "TestNativeImage"])')
            (output / "Package.swift").write_text(manifest)
    report = {
        "created_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "version": pin["version"], "engine_label": args.engine,
        "library": str(library), "library_sha256": digest(library),
        "source": str(source), "source_pin_sha256": digest(pin_path),
        "package": str(output), "manifest_sha256": digest(output / "Package.swift"),
        "copied_source_test_license_files": len(pin["files"]),
        "all_pinned_files_byte_identical": not (adaptations or source_adaptations), "bundled_engine_sources": False,
        "source_adaptations": source_adaptations,
        "fixture_adaptations": adaptations, "upstream_assertions_unchanged": True,
        "additional_test_files": additional_tests,
        "test_infrastructure_files": test_infrastructure,
        "build_or_tests_executed": False,
        "scope": "Prepared test package only; absolute linker flags are not portable distribution acceptance",
    }
    (output / "preparation.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))


if __name__ == "__main__":
    main()
