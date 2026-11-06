"""Create a relocatable macOS arm64 Swift package around an explicit Rust dylib."""

import argparse
import json
import plistlib
import shutil
import subprocess
from pathlib import Path

from prepare import digest
from adapt import apply_source_adaptations


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--library", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    source, library, out = args.source.resolve(), args.library.resolve(), args.out.resolve()
    if out.exists():
        parser.error("preserve evidence: output must be new")
    pin = json.loads((workspace / "swift/upstream.json").read_text())
    client = source / "tools/swift/duckdb-swift"
    header = source / "src/include/duckdb.h"
    assert digest(header) == pin["header_sha256"]
    for item in pin["files"]:
        assert digest(client / item["path"]) == item["sha256"], item["path"]
    subprocess.run(["codesign", "--verify", "--strict", str(library)], check=True)
    original_hash = digest(library)
    architecture = subprocess.check_output(["lipo", "-archs", str(library)], text=True).strip()
    assert architecture == "arm64", architecture
    commands = subprocess.check_output(["otool", "-l", str(library)], text=True)
    build = commands.split("cmd LC_BUILD_VERSION\n", 1)[1].split("Load command", 1)[0]
    assert "platform 1\n" in build, build  # PLATFORM_MACOS
    minimum = build.split("minos ", 1)[1].splitlines()[0].strip()
    out.mkdir(parents=True)
    package = out / "DuckDBRust"
    package.mkdir()
    for item in pin["files"]:
        if item["path"].startswith("Tests/"):
            continue
        destination = package / item["path"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(client / item["path"], destination)
    source_adaptations = apply_source_adaptations(package)
    framework = out / "framework/Cduckdb.framework"
    version = framework / "Versions/A"
    for name in ["Headers", "Modules", "Resources"]:
        (version / name).mkdir(parents=True, exist_ok=True)
    shutil.copy2(library, version / "Cduckdb")
    shutil.copy2(header, version / "Headers/duckdb.h")
    (version / "Modules/module.modulemap").write_text(
        'framework module Cduckdb {\n  umbrella header "duckdb.h"\n  export *\n}\n')
    metadata = {"CFBundleExecutable": "Cduckdb", "CFBundleIdentifier": "dev.rustdb.Cduckdb",
                "CFBundleName": "Cduckdb", "CFBundlePackageType": "FMWK",
                "CFBundleShortVersionString": pin["version"], "CFBundleVersion": pin["version"],
                "CFBundleSupportedPlatforms": ["MacOSX"], "MinimumOSVersion": minimum}
    (version / "Resources/Info.plist").write_bytes(plistlib.dumps(metadata))
    (framework / "Versions/Current").symlink_to("A")
    for name in ["Cduckdb", "Headers", "Modules", "Resources"]:
        (framework / name).symlink_to("Versions/Current/" + name)
    subprocess.run(["install_name_tool", "-id", "@rpath/Cduckdb.framework/Cduckdb",
                    str(version / "Cduckdb")], check=True)
    subprocess.run([str(workspace / "scripts/sign.sh"), str(framework)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(framework)], check=True)
    artifacts = package / "Artifacts"
    artifacts.mkdir()
    with (out / "xcframework.log").open("x") as log:
        subprocess.run(["nice", "-n", "10", "xcodebuild", "-create-xcframework", "-framework",
                        str(framework), "-output", str(artifacts / "Cduckdb.xcframework")],
                       stdout=log, stderr=subprocess.STDOUT, check=True)
    (package / "Package.swift").write_text('''// swift-tools-version: 5.7
import PackageDescription
let package = Package(
    name: "DuckDBRust",
    platforms: [.macOS("MINIMUM")],
    products: [.library(name: "DuckDB", targets: ["DuckDB"])],
    targets: [
        .binaryTarget(name: "Cduckdb", path: "Artifacts/Cduckdb.xcframework"),
        .target(name: "DuckDB", dependencies: ["Cduckdb"])
    ]
)
'''.replace("MINIMUM", minimum))
    assert digest(library) == original_hash, "original image changed"
    inventory = {str(p.relative_to(package)): digest(p) for p in sorted(package.rglob("*"))
                 if p.is_file() and not p.is_symlink()}
    report = {"package": str(package), "original_library": str(library),
              "original_library_sha256": original_hash, "architecture": architecture,
              "minimum_macos": minimum, "files": inventory,
              "source_adaptations": source_adaptations,
              "packaged_install_name": "@rpath/Cduckdb.framework/Cduckdb",
              "scope": "macOS arm64 package preparation; consumer verification pending"}
    (out / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: value for key, value in report.items() if key != "files"}))


if __name__ == "__main__":
    main()
