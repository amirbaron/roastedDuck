"""Synthetic rejection checks for the release gate; these are not release receipts."""
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from verify_qualification import ABIS, PLATFORMS, verify


class QualificationGateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.commit = "a" * 40
        self.manifest = {"distributions": []}
        for platform, wheel_platform in PLATFORMS.items():
            library = "libduckdb.dylib" if platform == "macos-arm64" else "libduckdb.so"
            native = hashlib.sha256(platform.encode()).hexdigest()
            bridge = hashlib.sha256((platform + "bridge").encode()).hexdigest()
            groups = ["crypto", "extension-signatures", "parquet-encryption", "persistence"]
            hashes = {"sdk/lib/" + library: native, "harnesses/adbc/" + library: bridge}
            hashes["sdk/include/adbc.h"] = "c" * 64
            hashes.update({"harnesses/" + name: hashlib.sha256(name.encode()).hexdigest() for name in groups})
            producer = self.root / ("candidate-native-manifest-" + platform) / "build-provenance.json"
            self.write(producer, {"source_commit": self.commit, "platform": platform, "export_count": 547,
                                  "adbc_test_api": False, "files_sha256": hashes})
            q = self.root / ("qualification-" + platform)
            self.write(q / "native-input-verification.json", {"passed": True, "source_commit": self.commit,
                       "producer_manifest_sha256": hashlib.sha256(producer.read_bytes()).hexdigest(),
                       "library_sha256": {"sdk/lib/" + library: native}})
            n = q / "native-qualification"
            self.write(n / "summary.json", [{"name": name, "exit_code": 0, "failed": 0, "ignored": 0,
                       "passed": 2, "binary_sha256": hashes["harnesses/" + name]} for name in groups])
            self.write(q / "qualification/summary.json", {"cells": [{"abi": abi} for abi in ABIS]})
            for abi in ABIS:
                name = abi + "-" + wheel_platform + ".whl"
                image = hashlib.sha256((name + "native").encode()).hexdigest()
                wheel = hashlib.sha256(name.encode()).hexdigest()
                self.manifest["distributions"].append({"abi": abi, "platform": wheel_platform,
                     "filename": name, "sha256": wheel, "native_sha256": image})
                cell = q / "qualification" / abi
                self.write(cell / "comparison.json", {"passed": True, "differences": {}, "explicit_outcome_difference": False,
                           "wheel": name, "engines": {engine: {"exit_code": 0, "counts": {"pass": 3455}} for engine in ["rust", "cpp"]}})
                self.write(cell / "rust/identity.json", {"sha256": image})
                self.write(cell / "rust/pytest-identity.json", {"native_sha256": image})
                self.write(cell / "rust/install.json", {"install": [{"download_info": {"archive_info": {"hashes": {"sha256": wheel}}}}]})
                self.write(cell / "rust/encryption.json", {"passed": True, "parquet_key_bits": [128, 192, 256],
                           "database_profiles": [{"cipher": c, "version": v, "checkpoint_reopen": True, "wrong_key_rejected": True}
                                                 for c in ["GCM", "CTR"] for v in ["v0", "v1"]]})
            for client, cases in [("capi", 155), ("go", 652), ("swift", 133)]:
                self.write(n / client / "verification.json", {"passed": True, "cases": cases, "native_sha256": native})
            self.write(n / "adbc/verification.json", {"passed": True, "test_only_bridge": True, "cases": 64,
                       "assertions": 3040, "test_bridge_sha256": bridge})
            self.write(n / "public-adbc/verification.json", {"passed": True, "test_only_bridge": False, "adbc_versions": ["1.0", "1.1"],
                "native_sha256": native, "public_header_sha256": "c" * 64,
                "checks": ["initialize", "connect", "query", "arrow-schema", "arrow-batch", "stream-end", "independent-arrow-ownership", "release"]})
            self.write(n / "input-manifest.json", {"rust_original_sha256": native, "artifacts": {"rust": {"sha256": native}}})
            for cipher in ["GCM", "CTR"]:
                for version in ["v0", "v1"]:
                    self.write(n / ("encrypted-" + cipher + "-" + version) / "verification.json", {
                        "cipher": cipher, "encryption_version": version, "journeys_pass": 4, "recovery_processes_pass": 8,
                        "native_processes": 22, "physical_encrypted_headers": [
                            {"file": str(i), "cipher_byte": {"GCM": 1, "CTR": 2}[cipher], "version_byte": int(version[1]),
                             "metadata_hex": bytes([1, 0, {"GCM": 1, "CTR": 2}[cipher], int(version[1]), 0, 0, 0, 0]).hex()}
                            for i in range(14)], "libraries": {"rust": {"sha256": native}}})

    @staticmethod
    def write(path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))

    def reject_change(self, relative, update):
        path = self.root / relative
        value = json.loads(path.read_text())
        update(value)
        self.write(path, value)
        with self.assertRaises(AssertionError):
            verify(self.root, self.manifest, self.commit)

    def test_complete_fixture(self):
        self.assertEqual(verify(self.root, self.manifest, self.commit)["python_cells"], 15)

    def test_missing_platform_wheel(self):
        self.manifest["distributions"].pop()
        with self.assertRaises(AssertionError):
            verify(self.root, self.manifest, self.commit)

    def test_wrong_runtime_image(self):
        self.reject_change("qualification-linux-arm64/native-qualification/go/verification.json", lambda v: v.update(native_sha256="0" * 64))

    def test_wrong_wheel_install(self):
        self.reject_change("qualification-macos-arm64/qualification/cp312/rust/install.json",
                           lambda v: v["install"][0]["download_info"]["archive_info"]["hashes"].update(sha256="0" * 64))

    def test_wrong_pytest_image(self):
        self.reject_change("qualification-linux-amd64/qualification/cp314/rust/pytest-identity.json", lambda v: v.update(native_sha256="0" * 64))

    def test_wrong_bridge(self):
        self.reject_change("qualification-linux-arm64/native-qualification/adbc/verification.json", lambda v: v.update(test_bridge_sha256="0" * 64))

    def test_bridge_substituted_for_public_adbc(self):
        self.reject_change("qualification-linux-arm64/native-qualification/public-adbc/verification.json", lambda v: v.update(test_only_bridge=True))

    def test_wrong_public_header(self):
        self.reject_change("qualification-linux-arm64/native-qualification/public-adbc/verification.json", lambda v: v.update(public_header_sha256="0" * 64))

    def test_missing_crypto_headers(self):
        self.reject_change("qualification-linux-arm64/native-qualification/encrypted-CTR-v1/verification.json", lambda v: v.update(physical_encrypted_headers=[]))

    def test_stale_source(self):
        self.reject_change("candidate-native-manifest-linux-amd64/build-provenance.json", lambda v: v.update(source_commit="b" * 40))

    def test_wrong_cipher_in_physical_header(self):
        self.reject_change("qualification-macos-arm64/native-qualification/encrypted-GCM-v0/verification.json",
                           lambda v: v["physical_encrypted_headers"][0].update(cipher_byte=2))

    def test_python_regression(self):
        self.reject_change("qualification-macos-arm64/qualification/cp310/comparison.json", lambda v: v.update(explicit_outcome_difference=True))


if __name__ == "__main__":
    unittest.main()
