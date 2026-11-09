"""Reject incomplete or non-equivalent installed-package audit inventories."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from audit_validation_environments import collect


class EnvironmentAuditTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for platform in ["linux-amd64", "linux-arm64", "macos-arm64"]:
            q = self.root / ("qualification-" + platform) / "qualification"
            q.mkdir(parents=True)
            cells = [{"abi": abi, "passed": True} for abi in ["cp310", "cp311", "cp312", "cp313", "cp314"]]
            (q / "summary.json").write_text(json.dumps({"cells": cells}))
            for cell in cells:
                for engine in ["cpp", "rust"]:
                    d = q / cell["abi"] / engine
                    d.mkdir(parents=True)
                    self.write(d, engine, "9.0.3")
        self.sample = self.root / "qualification-linux-amd64/qualification/cp310/rust"

    def write(self, directory, engine, pytest_version):
        packages = {"pytest": pytest_version, "duckdb" if engine == "cpp" else "roastedduck": "1.5.6"}
        (directory / "requirements.txt").write_text("".join(f"{k}=={v}\n" for k, v in packages.items()))
        (directory / "pip-inspect.json").write_text(json.dumps({"installed": [
            {"metadata": {"name": k, "version": v}} for k, v in packages.items()]}))

    def test_complete_thirty_environments(self):
        self.assertEqual(len(collect(self.root)), 30)

    def test_missing_inventory_rejected(self):
        (self.sample / "requirements.txt").unlink()
        with self.assertRaises(FileNotFoundError): collect(self.root)

    def test_omitted_installed_dependency_rejected(self):
        (self.sample / "requirements.txt").write_text("roastedduck==1.5.6\n")
        with self.assertRaises(AssertionError): collect(self.root)

    def test_different_resolved_environment_rejected(self):
        self.write(self.sample, "rust", "9.0.4")
        with self.assertRaises(AssertionError): collect(self.root)


if __name__ == "__main__":
    unittest.main()
