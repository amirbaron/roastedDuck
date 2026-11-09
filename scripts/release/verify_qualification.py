#!/usr/bin/env python3
"""Reconcile raw CI receipts with every producer image and Python distribution."""
import argparse
import hashlib
import json
from pathlib import Path

PLATFORMS = {"macos-arm64": "macosx_11_0_arm64", "linux-arm64": "manylinux_2_28_aarch64", "linux-amd64": "manylinux_2_28_x86_64"}
ABIS = {"cp310", "cp311", "cp312", "cp313", "cp314"}

def read(path):
    return json.loads(path.read_text())

def verify(inputs, manifest, commit):
    wheels = [item for item in manifest["distributions"] if "abi" in item]
    assert len(wheels) == 15 and {(item["platform"], item["abi"]) for item in wheels} == {(platform, abi) for platform in PLATFORMS.values() for abi in ABIS}
    expected_native = {}
    for platform, wheel_platform in PLATFORMS.items():
        producer_file = inputs / ("candidate-native-manifest-" + platform) / "build-provenance.json"
        producer = read(producer_file)
        assert producer["source_commit"] == commit and producer["platform"] == platform
        assert producer["export_count"] == 547 and producer["adbc_test_api"] is False
        library_name = "libduckdb.dylib" if platform == "macos-arm64" else "libduckdb.so"
        native_hash = producer["files_sha256"]["sdk/lib/" + library_name]
        bridge_hash = producer["files_sha256"]["harnesses/adbc/" + library_name]
        expected_native[platform] = native_hash
        qualification = inputs / ("qualification-" + platform)
        native = qualification / "native-qualification"
        verification = read(qualification / "native-input-verification.json")
        assert verification["passed"] is True and verification["source_commit"] == commit
        assert verification["producer_manifest_sha256"] == hashlib.sha256(producer_file.read_bytes()).hexdigest()
        assert verification["library_sha256"]["sdk/lib/" + library_name] == native_hash
        harnesses = read(native / "summary.json")
        assert {item["name"] for item in harnesses} == {"crypto", "extension-signatures", "parquet-encryption", "persistence"}
        for item in harnesses:
            assert item["exit_code"] == item["failed"] == item["ignored"] == 0 and item["passed"] > 0
            assert item["binary_sha256"] == producer["files_sha256"]["harnesses/" + item["name"]]
        cells = read(qualification / "qualification/summary.json")["cells"]
        assert len(cells) == 5 and {cell["abi"] for cell in cells} == ABIS
        for wheel in [item for item in wheels if item["platform"] == wheel_platform]:
            cell = qualification / "qualification" / wheel["abi"]
            comparison = read(cell / "comparison.json")
            assert comparison["passed"] is True and comparison["differences"] == {} and comparison["explicit_outcome_difference"] is False
            assert comparison["wheel"] == wheel["filename"]
            for engine in ["rust", "cpp"]:
                assert comparison["engines"][engine]["exit_code"] == 0
                assert comparison["engines"][engine]["counts"]["pass"] > 3000
            for file, key in [("identity.json", "sha256"), ("pytest-identity.json", "native_sha256")]:
                assert read(cell / "rust" / file)[key] == wheel["native_sha256"]
            installed = read(cell / "rust/install.json")["install"]
            assert len(installed) == 1 and installed[0]["download_info"]["archive_info"]["hashes"]["sha256"] == wheel["sha256"]
            encryption = read(cell / "rust/encryption.json")
            assert encryption["passed"] is True and encryption["parquet_key_bits"] == [128, 192, 256]
            assert {(item["cipher"], item["version"]) for item in encryption["database_profiles"]} == {(c, v) for c in ["GCM", "CTR"] for v in ["v0", "v1"]}
            assert all(item["checkpoint_reopen"] and item["wrong_key_rejected"] for item in encryption["database_profiles"])
        for client, cases in [("capi", 155), ("go", 652), ("swift", 133)]:
            receipt = read(native / client / "verification.json")
            assert receipt["passed"] is True and receipt["cases"] == cases and receipt["native_sha256"] == native_hash
        adbc = read(native / "adbc/verification.json")
        assert adbc["passed"] is True and adbc["test_only_bridge"] is True and adbc["cases"] == 64 and adbc["assertions"] == 3040
        assert adbc["test_bridge_sha256"] == bridge_hash
        encrypted_input = read(native / "input-manifest.json")
        assert encrypted_input["rust_original_sha256"] == encrypted_input["artifacts"]["rust"]["sha256"] == native_hash
        for cipher in ["GCM", "CTR"]:
            for version in ["v0", "v1"]:
                receipt = read(native / ("encrypted-" + cipher + "-" + version) / "verification.json")
                assert receipt["cipher"] == cipher and receipt["encryption_version"] == version
                assert receipt["journeys_pass"] == 4 and receipt["recovery_processes_pass"] == 8 and receipt["native_processes"] == 22
                assert len(receipt["physical_encrypted_headers"]) == 14
                assert len({header["file"] for header in receipt["physical_encrypted_headers"]}) == 14
                for header in receipt["physical_encrypted_headers"]:
                    assert header["cipher_byte"] == {"GCM": 1, "CTR": 2}[cipher]
                    assert header["version_byte"] == {"v0": 0, "v1": 1}[version]
                    metadata = bytes.fromhex(header["metadata_hex"])
                    assert len(metadata) == 8 and metadata[0] == 1
                    assert metadata[2] == header["cipher_byte"] and metadata[3] == header["version_byte"]
                assert receipt["libraries"]["rust"]["sha256"] == native_hash
    return {"passed": True, "source_commit": commit, "native_images": expected_native,
            "python_images": {item["filename"]: item["native_sha256"] for item in wheels},
            "python_cells": 15, "native_platforms": sorted(PLATFORMS)}

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    result = verify(args.inputs, read(args.manifest), args.commit)
    args.out.write_text(json.dumps(result, indent=2) + "\n")
    print("Verified all 15 Python cells and three native client/crypto platforms")
