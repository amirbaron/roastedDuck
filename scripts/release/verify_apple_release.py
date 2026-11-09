#!/usr/bin/env python3
"""Verify signed Apple payloads and runtime evidence without executing native code."""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import re
import struct
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/signing"))
from verify_detached_manifest import verify as verify_signature

CERTIFICATE = "a1354a5ea7a95b11430e91e216689c84e8c1a6392b4c06466199b87217cb709d"
SLICES = {
    "macos-arm64": ("macos-arm64/Cduckdb.framework/Versions/A/Cduckdb", 1),
    "ios-arm64": ("ios-arm64/Cduckdb.framework/Cduckdb", 2),
    "ios-simulator-arm64": ("ios-arm64-simulator/Cduckdb.framework/Cduckdb", 7),
}

def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

def engine_inputs(commit):
    assert re.fullmatch(r"[0-9a-f]{40}", commit), "A full immutable source commit is required"
    roots = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates", "third_party", ".cargo", "swift"}
    available = set(subprocess.check_output(["git", "ls-tree", "--name-only", commit], cwd=ROOT, text=True).splitlines())
    # The Apple module consumes duckdb.h. The separately distributed ADBC header
    # is neither an Apple SDK payload nor an input to its compiled native image.
    command = ["git", "archive", commit, *sorted(roots & available), "include/duckdb.h"]
    process = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE)
    inputs = {}
    with tarfile.open(fileobj=process.stdout, mode="r|") as archive:
        for entry in archive:
            if entry.isfile():
                inputs[entry.name] = hashlib.file_digest(archive.extractfile(entry), "sha256").hexdigest()
            else:
                assert entry.isdir(), "Unexpected source link: " + entry.name
    assert process.wait() == 0 and inputs
    return inputs

def verify(root, commit, temporary):
    root = root.resolve()
    temporary.mkdir(parents=True, exist_ok=True)
    verify_signature(root / "SHA256SUMS", root / "SHA256SUMS.csig", root / "rustdb-dev.pem", "openssl", temporary, CERTIFICATE)
    listed = {line[66:] for line in (root / "SHA256SUMS").read_text().splitlines()}
    actual = {str(path.relative_to(root)) for path in root.rglob("*") if path.is_file() and not path.is_symlink()}
    assert actual == listed | {"SHA256SUMS", "SHA256SUMS.csig"}, "Apple archive contains unsigned extras or missing files"
    links = {str(path.relative_to(root)): str(path.readlink()) for path in root.rglob("*") if path.is_symlink()}
    assert links == json.loads((root / "symlinks.json").read_text())
    assert all((root / name).resolve().is_relative_to(root) for name in links)
    metadata = json.loads((root / "apple-release.json").read_text())
    assert metadata["compatibility_version"] == "1.5.6"
    assert metadata["physical_device_verified"] is False
    expected_inputs = engine_inputs(commit)
    assert metadata["engine_inputs"] == expected_inputs
    assert engine_inputs(metadata["engine_source_identity_commit"]) == expected_inputs
    images = {}
    xcframework = root / "DuckDBRust/Artifacts/Cduckdb.xcframework"
    declarations = plistlib.loads((xcframework / "Info.plist").read_bytes())["AvailableLibraries"]
    assert len(declarations) == 3
    assert {(item["LibraryIdentifier"], item["SupportedPlatform"], item.get("SupportedPlatformVariant"), tuple(item["SupportedArchitectures"])) for item in declarations} == {
        ("macos-arm64", "macos", None, ("arm64",)), ("ios-arm64", "ios", None, ("arm64",)),
        ("ios-arm64-simulator", "ios", "simulator", ("arm64",))}
    for platform, (relative, expected_platform) in SLICES.items():
        path = root / "DuckDBRust/Artifacts/Cduckdb.xcframework" / relative
        data = path.read_bytes()
        base = 0
        magic = struct.unpack_from(">I", data)[0]
        if magic in (0xCAFEBABE, 0xCAFEBABF):
            assert struct.unpack_from(">I", data, 4)[0] == 1, "Only the selected ARM architecture may ship"
            base = struct.unpack_from(">I" if magic == 0xCAFEBABE else ">Q", data, 16)[0]
        assert data[base:base + 4] == b"\xcf\xfa\xed\xfe" and struct.unpack_from("<I", data, base + 4)[0] == 0x100000C
        offset, platforms = base + 32, []
        for _ in range(struct.unpack_from("<I", data, base + 16)[0]):
            command, size = struct.unpack_from("<II", data, offset)
            assert size >= 8 and offset + size <= len(data)
            if command == 0x32:
                platforms.append(struct.unpack_from("<I", data, offset + 8)[0])
            offset += size
        assert platforms == [expected_platform]
        images[platform] = sha(path)
    assert metadata["native_images"] == images
    def receipt(name):
        return json.loads((root / "evidence" / (name + ".json")).read_text())
    mac, sim = receipt("mac-suite"), receipt("simulator-suite")
    assert mac["passed"] is True and mac["cases"] == 133 and mac["native_sha256"] == images["macos-arm64"]
    assert sim["exit_code"] == 0 and sim["started"] == sim["passed"] == 133 and sim["failed"] == []
    assert sim["same_case_ids"] is True and sim["runtime_identity_verified"] is True and sim["test_sections_unchanged"] is True
    assert sim["physical_device_verified"] is False and sim["new_framework_sha256"] == images["ios-simulator-arm64"]
    assert len(set(mac["case_ids"])) == 133 and set(mac["case_ids"]) == set(sim["case_ids"])
    encryption = receipt("simulator-encryption")
    expected_checks = {c + "-" + v + "-checkpoint-reopen-wrong-key" for c in ["GCM", "CTR"] for v in ["v0", "v1"]}
    assert encryption["passed"] is True and encryption["native_sha256"] == images["ios-simulator-arm64"]
    assert set(encryption["checks"]) == expected_checks and encryption["format_headers_verified"] == 4
    assert encryption["physical_device_verified"] is False
    mac_encryption = receipt("mac-encryption")
    assert mac_encryption["build_exit_code"] == mac_encryption["run_exit_code"] == 0
    assert mac_encryption["loaded_framework_sha256"] == images["macos-arm64"]
    assert set(mac_encryption["consumer"]["checks"]) == expected_checks
    deployed = receipt("mac-deployed")
    assert deployed["exit_code"] == 0 and deployed["framework_sha256"] == images["macos-arm64"]
    interop = receipt("mac-interop")
    assert interop["passed_apple_swift_typed_persistence_gate"] is True and interop["journeys"] == 4
    assert interop["native_images"]["rust"]["sha256"] == images["macos-arm64"]
    assert {(case["writer"], case["reader"], case["kind"]) for case in interop["cases"]} == {
        (writer, reader, kind) for writer, reader in [("cpp", "rust"), ("rust", "cpp")] for kind in ["checkpoint", "wal"]}
    device = receipt("device-build")
    assert device["build_exit_code"] == 0 and device["build_only"] is True and device["runtime_verified"] is False
    assert device["run_exit_code"] is None and device["build_version"]["platform"] == 2
    assert device["original_and_relocated_package_hashes_unchanged"] is True
    device_identity = receipt("device-input-identity")
    assert device_identity["observed_after_build"] is True and device_identity["runtime_verified"] is False
    assert device_identity["selected_framework_sha256"] == images["ios-arm64"]
    assert device_identity["consumer_executable_sha256"] == device["executable_sha256"]
    assert device_identity["device_build_receipt_sha256"] == sha(root / "evidence/device-build.json")
    return {"passed": True, "native_images": images, "engine_inputs_match_commit": commit,
            "engine_source_identity_commit": metadata["engine_source_identity_commit"], "mac_cases": 133, "simulator_cases": 133,
            "physical_device_verified": False, "certificate_sha256": CERTIFICATE}

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--tmp", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.write_text(json.dumps(verify(args.root, args.commit, args.tmp), indent=2) + "\n")
    print("Signed Apple images and Mac/simulator evidence verified; physical iOS remains unverified")
