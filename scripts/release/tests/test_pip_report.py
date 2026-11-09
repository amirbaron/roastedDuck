"""Accept real pip schema evolution without weakening artifact identity checks."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from pip_report import archive_sha256


class PipReportTests(unittest.TestCase):
    DIGEST = "eb448126e745fe9af115a8d29aa56f32a1c4d2f9e6d168eecd4b9c3bb5c1ce45"

    def test_pip_23_legacy_report(self):
        self.assertEqual(archive_sha256({"hash": "sha256=" + self.DIGEST}), self.DIGEST)

    def test_current_report(self):
        self.assertEqual(archive_sha256({"hashes": {"sha256": self.DIGEST}}), self.DIGEST)

    def test_consistent_dual_representation(self):
        self.assertEqual(archive_sha256({"hash": "sha256=" + self.DIGEST, "hashes": {"sha256": self.DIGEST}}), self.DIGEST)

    def test_conflicting_representations_rejected(self):
        with self.assertRaises(AssertionError):
            archive_sha256({"hash": "sha256=" + "0" * 64, "hashes": {"sha256": self.DIGEST}})

    def test_missing_digest_rejected(self):
        with self.assertRaises(AssertionError):
            archive_sha256({})

    def test_weaker_legacy_digest_rejected(self):
        with self.assertRaises(AssertionError):
            archive_sha256({"hash": "md5=" + "0" * 32})

    def test_invalid_sha256_rejected(self):
        for digest in ["0" * 63, "z" * 64, None]:
            with self.subTest(digest=digest), self.assertRaises(AssertionError):
                archive_sha256({"hashes": {"sha256": digest}})


if __name__ == "__main__":
    unittest.main()
