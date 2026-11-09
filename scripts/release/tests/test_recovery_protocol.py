"""Recovery completes missing platforms; it never converts failures into passes."""
import copy
import json
from pathlib import Path
import sys
import unittest
from tempfile import TemporaryDirectory
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from observer_protocol import REQUIRED_JOBS
from recovery_protocol import RECOVERED_JOBS, validate_recovery, verify_workflow, verify_input_receipts


class RecoveryProtocolTests(unittest.TestCase):
    def setUp(self):
        self.config = {"repository": "amirbaron/roastedDuck", "producer_run": 123,
                       "candidate_source_commit": "a" * 40, "provider_error": "Recorded internal server error"}
        self.pin = {"run": 456, "commit": "b" * 40}
        self.run = {"id": 123, "run_attempt": 1, "head_sha": "a" * 40,
                    "head_repository": {"full_name": self.config["repository"]},
                    "path": ".github/workflows/build-candidate.yml", "status": "completed", "conclusion": "failure"}
        self.jobs = [{"id": i, "name": n, "status": "completed", "conclusion": "success"}
                     for i, n in enumerate(sorted(REQUIRED_JOBS - RECOVERED_JOBS))]
        self.jobs.append({"id": 99, "name": "assemble", "status": "completed", "conclusion": "skipped"})
        self.recovered = dict(self.run, id=456, head_sha="b" * 40,
                              path=".github/workflows/recover-qualification.yml", conclusion="success")
        self.recovered_jobs = [{"id": i + 100, "name": n, "status": "completed", "conclusion": "success"}
                               for i, n in enumerate(sorted(RECOVERED_JOBS))]

    def verify(self):
        return validate_recovery(self.run, self.jobs, self.recovered, self.recovered_jobs, self.config, self.pin)

    def test_complete_recovery_keeps_original_failure_and_skipped_aggregate(self):
        result = self.verify()
        self.assertEqual(result["producer_run_conclusion"], "failure")
        self.assertEqual(result["original_aggregate_diagnosis"]["conclusion"], "skipped")
        self.assertEqual(len(result["required_producer_jobs"]), 6)
        self.assertEqual(len(result["qualification_recovery_jobs"]), 2)

    def test_original_failure_is_not_excused(self):
        self.jobs[0]["conclusion"] = "failure"
        with self.assertRaises(AssertionError): self.verify()

    def test_missing_native_build_is_not_excused(self):
        self.jobs.pop(0)
        with self.assertRaises(AssertionError): self.verify()

    def test_failed_recovery_is_rejected(self):
        self.recovered_jobs[0]["conclusion"] = "failure"
        with self.assertRaises(AssertionError): self.verify()

    def test_incomplete_recovery_is_rejected(self):
        self.recovered_jobs.pop()
        with self.assertRaises(AssertionError): self.verify()

    def test_wrong_recovery_source_or_workflow_is_rejected(self):
        for key, value in [("head_sha", "c" * 40), ("path", ".github/workflows/build-candidate.yml"), ("id", 999)]:
            saved = self.recovered[key]; self.recovered[key] = value
            with self.assertRaises(AssertionError): self.verify()
            self.recovered[key] = saved

    def test_previously_failed_platform_cannot_be_hidden(self):
        self.jobs.append(dict(self.recovered_jobs[0], conclusion="failure"))
        with self.assertRaises(AssertionError): self.verify()

    def test_unknown_aggregate_failure_is_rejected(self):
        self.jobs[-1]["conclusion"] = "failure"
        with self.assertRaises(AssertionError): self.verify()

    def test_duplicate_job_is_rejected(self):
        self.recovered_jobs.append(copy.deepcopy(self.recovered_jobs[0]))
        with self.assertRaises(AssertionError): self.verify()

    def test_changed_runtime_steps_are_rejected(self):
        marker = "      - name: Run the compiled security and persistence tests natively\n"
        with patch("recovery_protocol.subprocess.check_output", side_effect=[marker + "original\n  assemble:\n", marker + "changed\n      - name: Retain immutable input identity\n"]):
            with self.assertRaises(AssertionError): verify_workflow("a" * 40, "b" * 40)

    def test_input_receipts_bind_the_actual_original_artifacts(self):
        evidence = self.verify()
        pins = {p: {"id": i, "name": "candidate-" + p, "digest": "sha256:" + "d" * 64, "size_in_bytes": 123}
                for i, p in enumerate(["linux-amd64", "macos-arm64"], 1)}
        with TemporaryDirectory() as temporary, patch("recovery_protocol.recovery_config", return_value=(self.pin, {"artifacts": pins})):
            root = Path(temporary)
            for platform, pin in pins.items():
                directory = root / ("recovery-input-" + platform); directory.mkdir()
                receipt = {"passed": True, "distributions_rebuilt": False, "source_commit": "a" * 40,
                           "producer_run": 123, "recovery_run": 456, "verification_tools_commit": "b" * 40,
                           "platform": platform, "artifact": pin}
                (directory / "recovery-input.json").write_text(json.dumps(receipt))
            verify_input_receipts(root, evidence, list(pins.values()))
            file = root / "recovery-input-macos-arm64/recovery-input.json"
            original = json.loads(file.read_text())
            for key, value in [("source_commit", "c" * 40), ("recovery_run", 999),
                               ("verification_tools_commit", "c" * 40), ("platform", "linux-amd64"),
                               ("artifact", dict(pins["macos-arm64"], digest="sha256:" + "e" * 64))]:
                file.write_text(json.dumps(dict(original, **{key: value})))
                with self.assertRaises(AssertionError): verify_input_receipts(root, evidence, list(pins.values()))


if __name__ == "__main__":
    unittest.main()
