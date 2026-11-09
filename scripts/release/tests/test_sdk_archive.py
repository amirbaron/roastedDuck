"""Ensure shipping archives cannot omit or substitute non-DLL SDK payloads."""
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from archive_native_sdk import sha, verify_archive


class SDKArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.sdk = self.root / "sdk"
        for name in ["bin/rustdb", "lib/libduckdb.so", "lib/libduckdb.a", "include/adbc.h", "notices/project/crates/common/data/utf8proc-LICENSE"]:
            path = self.sdk / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("synthetic fixture: " + name)
        (self.sdk / "bin/roastedDuck").symlink_to("rustdb")
        self.provenance = {"files_sha256": {"sdk/" + str(p.relative_to(self.sdk)): sha(p)
                           for p in self.sdk.rglob("*") if p.is_file() and not p.is_symlink()}}

    def archive(self):
        path = self.root / "sdk.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            archive.add(self.sdk, arcname=".")
        self.provenance["sdk_archive"] = {"sha256": sha(path)}
        return path

    def test_all_files_verified(self):
        self.assertEqual(verify_archive(self.archive(), self.provenance)["files_verified"], 5)

    def test_static_library_substitution(self):
        (self.sdk / "lib/libduckdb.a").write_text("different static library")
        with self.assertRaises(AssertionError):
            verify_archive(self.archive(), self.provenance)

    def test_missing_cli(self):
        (self.sdk / "bin/rustdb").unlink()
        with self.assertRaises(AssertionError):
            verify_archive(self.archive(), self.provenance)

    def test_unapproved_extra_file(self):
        (self.sdk / "lib/private-adbc-bridge.so").write_text("test-only bridge")
        with self.assertRaises(AssertionError):
            verify_archive(self.archive(), self.provenance)

    def test_wrong_alias(self):
        (self.sdk / "bin/roastedDuck").unlink()
        (self.sdk / "bin/roastedDuck").symlink_to("../lib/libduckdb.so")
        with self.assertRaises(AssertionError):
            verify_archive(self.archive(), self.provenance)

    def use_branded_layout(self):
        (self.sdk / "bin/roastedDuck").unlink()
        (self.sdk / "bin/rustdb").rename(self.sdk / "bin/roastedDuck")
        self.provenance["files_sha256"]["sdk/bin/roastedDuck"] = self.provenance["files_sha256"].pop("sdk/bin/rustdb")
        self.provenance["sdk_layout"] = "roastedduck-v2"

    def test_branded_cli_is_a_real_file(self):
        self.use_branded_layout()
        self.assertTrue(verify_archive(self.archive(), self.provenance)["passed"])

    def test_branded_layout_rejects_legacy_alias(self):
        self.use_branded_layout()
        (self.sdk / "bin/rustdb").symlink_to("roastedDuck")
        with self.assertRaises(AssertionError):
            verify_archive(self.archive(), self.provenance)

    def test_unknown_layout_rejected(self):
        self.provenance["sdk_layout"] = "unknown"
        with self.assertRaises(AssertionError):
            verify_archive(self.archive(), self.provenance)


if __name__ == "__main__":
    unittest.main()
