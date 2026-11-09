"""Only an understood aggregate-parser failure may use the observer path."""
import copy
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from observer_protocol import REQUIRED_JOBS, validate_producer, verification_only_changes


class ObserverProtocolTests(unittest.TestCase):
    def setUp(self):
        self.commit = "a" * 40
        self.repo = "amirbaron/roastedDuck"
        self.run = {"id": 123, "run_attempt": 1, "head_sha": self.commit,
                    "head_repository": {"full_name": self.repo}, "path": ".github/workflows/build-candidate.yml",
                    "status": "completed", "conclusion": "failure"}
        self.jobs = [{"id": i, "name": name, "status": "completed", "conclusion": "success", "steps": []}
                     for i, name in enumerate(sorted(REQUIRED_JOBS), 1)]
        self.jobs.append({"id": 9, "name": "assemble", "status": "completed", "conclusion": "failure",
                          "steps": [{"name": "Run python scripts/release/verify_qualification.py", "conclusion": "failure"}]})
        self.log = "scripts/release/verify_qualification.py\nKeyError: 'hashes'\n"

    def verify(self):
        return validate_producer(self.run, self.jobs, self.repo, self.commit, self.log)

    def test_known_parser_failure_remains_reported_as_failure(self):
        result = self.verify()
        self.assertEqual(result["producer_run_conclusion"], "failure")
        self.assertEqual(result["original_aggregate_diagnosis"]["error"], "KeyError: 'hashes'")

    def test_successful_producer_is_supported(self):
        self.run["conclusion"] = self.jobs[-1]["conclusion"] = "success"
        self.jobs[-1]["steps"] = []
        self.assertIsNone(self.verify()["original_aggregate_diagnosis"])

    def test_failed_runtime_cannot_be_accepted(self):
        next(j for j in self.jobs if j["name"].startswith("qualify"))["conclusion"] = "failure"
        with self.assertRaises(AssertionError):
            self.verify()

    def test_missing_platform_cannot_be_accepted(self):
        self.jobs.pop(0)
        with self.assertRaises(AssertionError):
            self.verify()

    def test_unknown_aggregate_failure_cannot_be_accepted(self):
        self.log = "verify_qualification.py\nAssertionError: wrong native image"
        with self.assertRaises(AssertionError):
            self.verify()

    def test_other_failed_step_cannot_be_accepted(self):
        self.jobs[-1]["steps"][0]["name"] = "Run twine check"
        with self.assertRaises(AssertionError):
            self.verify()

    def test_cancelled_producer_cannot_be_accepted(self):
        self.run["conclusion"] = "cancelled"
        with self.assertRaises(AssertionError):
            self.verify()

    def test_wrong_source_cannot_be_accepted(self):
        self.run["head_sha"] = "b" * 40
        with self.assertRaises(AssertionError):
            self.verify()

    def test_runtime_source_change_requires_new_build(self):
        with patch("observer_protocol.subprocess.check_output", return_value="Cargo.lock\n"):
            with self.assertRaises(AssertionError):
                verification_only_changes("a" * 40, "b" * 40)

    def test_only_explicit_observer_paths_allowed(self):
        with patch("observer_protocol.subprocess.check_output", return_value="scripts/release/pip_report.py\n"):
            self.assertEqual(verification_only_changes("a" * 40, "b" * 40), ["scripts/release/pip_report.py"])


if __name__ == "__main__":
    unittest.main()
