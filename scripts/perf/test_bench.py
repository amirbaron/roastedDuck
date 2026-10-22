"""Benchmark evidence must reject incorrect results and failed engines."""
import contextlib
import io
import json
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest
from unittest.mock import patch

import bench


class BenchmarkValidationTest(unittest.TestCase):
    def measure(self, results, *options):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "result.json"
            argv = ["bench.py", "test", "--out", str(output), *options]
            stdout = io.StringIO()
            with (patch.object(bench.sys, "argv", argv),
                  patch.object(bench, "suite_queries", return_value=[("q01", "db", "SELECT 1", None)]),
                  patch.object(bench, "interpreter_rss", return_value=0),
                  patch.object(bench, "run", side_effect=lambda engine, *args: dict(results[engine])),
                  patch.object(bench, "tie_check", return_value=False),
                  contextlib.redirect_stdout(stdout)):
                try:
                    bench.main()
                    failure = None
                except SystemExit as error:
                    failure = error.code
            return failure, json.loads(output.read_text()), stdout.getvalue()

    @staticmethod
    def timed(result):
        return {"wall": 1.0, "cpu": 1.0, "rss": 100, "result": result}

    def test_matching_results_keep_timings_and_verdict(self):
        failure, data, _ = self.measure({"duck": self.timed("1"), "rust": self.timed("1")})
        self.assertIsNone(failure)
        self.assertEqual(data["rows"]["q01"]["check"], "ok")
        self.assertEqual(data["rows"]["q01"]["speed"], 1.0)

    def test_mismatch_fails_and_excludes_timing(self):
        failure, data, text = self.measure({"duck": self.timed("1"), "rust": self.timed("2")})
        self.assertIn("validation failed: q01", failure)
        self.assertEqual(data["rows"]["q01"]["check"], "DIFF")
        self.assertNotIn("speed", data["rows"]["q01"])
        self.assertNotIn("geomean speed", text)

    def test_engine_error_fails_and_retains_error(self):
        failure, data, _ = self.measure({"duck": self.timed("1"), "rust": {"error": "query failed"}})
        self.assertIn("validation failed: q01", failure)
        self.assertEqual(data["rows"]["q01"]["check"], "error")
        self.assertEqual(data["rows"]["q01"]["rust"]["error"], "query failed")

    def test_reordered_results_retain_verdict(self):
        failure, data, _ = self.measure({"duck": self.timed("1\n2"), "rust": self.timed("2\n1")})
        self.assertIsNone(failure)
        self.assertEqual(data["rows"]["q01"]["check"], "ok (order)")

    def test_table_rejects_failed_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "failed.json"
            output.write_text(json.dumps({"rows": {"q01": {"check": "DIFF"}}}))
            result = subprocess.run(
                [sys.executable, "-I", str(Path(bench.__file__).with_name("table.py")), "--summary", str(output)],
                capture_output=True, text=True, check=False,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("benchmark validation failed: q01", result.stderr)

    def test_unchecked_measurements_are_labelled(self):
        failure, data, _ = self.measure({"duck": self.timed("1"), "rust": self.timed("2")}, "--no-check")
        self.assertIsNone(failure)
        self.assertEqual(data["rows"]["q01"]["check"], "unchecked")


if __name__ == "__main__":
    unittest.main()
