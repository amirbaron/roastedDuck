"""Prepare the full pinned Swift tests against an authenticated Apple SDK.

This creates inputs only. It does not compile, boot a simulator, or run tests.
The independent XCTest identity case accepts the SDK's Cduckdb framework name.
"""

import argparse
import datetime
import hashlib
import json
from pathlib import Path
import shutil

from package_identity import package_files, symlinks, verify_sdk


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--sdk", required=True, type=Path)
    parser.add_argument("--certificate-sha256", required=True)
    parser.add_argument("--tmpdir", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    source, sdk, output = args.source.resolve(), args.sdk.resolve(), args.out.resolve()
    if output.exists():
        parser.error("preserve previous evidence; --out must be fresh")
    if not args.certificate_sha256:
        parser.error("an independently trusted certificate pin is required")
    authenticated = verify_sdk(sdk, args.certificate_sha256, args.tmpdir)
    pin_path = workspace / "swift/upstream.json"
    pin = json.loads(pin_path.read_text())
    client = source / "tools/swift/duckdb-swift"
    assert digest(source / "src/include/duckdb.h") == pin["header_sha256"]
    for item in pin["files"]:
        assert digest(client / item["path"]) == item["sha256"], item["path"]
        if item["path"].startswith("Sources/DuckDB/"):
            assert digest(sdk / "DuckDBRust" / item["path"]) == item["sha256"], item["path"]
    identity = workspace / "scripts/swift-tests/LibraryIdentityTests.swift"
    output.mkdir(parents=True)
    copied_sdk = output / "sdk"
    shutil.copytree(sdk, copied_sdk, symlinks=True)
    assert package_files(copied_sdk / "DuckDBRust") == package_files(sdk / "DuckDBRust")
    assert symlinks(copied_sdk / "DuckDBRust") == symlinks(sdk / "DuckDBRust")
    assert verify_sdk(copied_sdk, args.certificate_sha256, args.tmpdir) == authenticated
    package = output / "Suite"
    package.mkdir()
    tests = []
    for item in pin["files"]:
        if not item["path"].startswith("Tests/"):
            continue
        destination = package / item["path"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(client / item["path"], destination)
        assert digest(destination) == item["sha256"]
        tests.append(item)
    assert len(tests) == 9, "the complete pinned upstream test-file set is required"
    destination = package / "Tests/DuckDBTests/LibraryIdentityTests.swift"
    shutil.copy2(identity, destination)
    manifest = '''// swift-tools-version: 5.7
import PackageDescription

let package = Package(
    name: "DuckDBReleaseTests",
    platforms: [.macOS(.v11), .iOS(.v15)],
    dependencies: [.package(name: "DuckDBRust", path: "../sdk/DuckDBRust")],
    targets: [
        .testTarget(name: "DuckDBTests", dependencies: [
            .product(name: "DuckDB", package: "DuckDBRust")
        ])
    ]
)
'''
    (package / "Package.swift").write_text(manifest)
    assert not list(package.rglob("*.cpp"))
    assert not list(package.rglob("*.cxx"))
    # No fixture adaptations: the tests retain their original Foundation paths
    # within the dedicated, internally stored simulator's application sandbox.
    report = {
        "created_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "source": str(source),
        "source_pin_sha256": digest(pin_path),
        "original_sdk": str(sdk),
        "sdk": str(copied_sdk),
        "certificate_sha256": args.certificate_sha256,
        "sdk_authenticated_payloads": authenticated,
        "sdk_manifest_sha256": digest(copied_sdk / "SHA256SUMS"),
        "package": str(package),
        "manifest_sha256": digest(package / "Package.swift"),
        "upstream_test_files": tests,
        "upstream_test_files_byte_identical": True,
        "additional_identity_test_sha256": digest(destination),
        "expected_upstream_case_count": 132,
        "expected_additional_case_count": 1,
        "fixture_adaptations": [],
        "runtime_expectations": {
            "RUSTDB_SWIFT_EXPECTED_LIBRARY": "exact installed Cduckdb.framework/Cduckdb path",
            "RUSTDB_SWIFT_IDENTITY_REPORT": "fresh JSON path inside the dedicated simulator",
        },
        "build_or_tests_executed": False,
        "scope": "Complete pinned Swift suite inputs only; simulator build, signing, execution, case coverage and physical-device verification remain required",
    }
    (output / "preparation.json").write_text(json.dumps(report, indent=2) + "\n")
    assert verify_sdk(sdk, args.certificate_sha256, args.tmpdir) == authenticated
    print(json.dumps({key: value for key, value in report.items() if key != "upstream_test_files"}))


if __name__ == "__main__":
    main()
