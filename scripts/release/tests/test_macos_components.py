"""Reject mixed-source components and incomplete Mac release coverage."""
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from join_macos_candidate import REPOSITORY, verify_sources, verify_wheel_names
from observer_protocol import REQUIRED_JOBS, producer_job_names


class MacComponentsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.record = {'commit': 'a' * 40, 'tree': 'b' * 40, 'github_run_id': '123',
                       'github_repository': REPOSITORY, 'cargo_build_jobs': '1',
                       'cargo_profile': 'release (repository-defined; no LTO or optimization overrides)',
                       'source_sha256': {'Cargo.lock': 'c' * 64}}
        self.native = self.root / 'native.json'
        self.wheels = self.root / 'wheels.json'
        self.native.write_text(json.dumps(self.record))
        self.wheels.write_bytes(self.native.read_bytes())

    def verify(self):
        return verify_sources(self.native, self.wheels, 'a' * 40, 123)

    def test_identical_same_run_receipts_pass(self):
        self.assertEqual(self.verify(), self.record)

    def test_different_input_closures_rejected(self):
        changed = dict(self.record, source_sha256={'Cargo.lock': 'd' * 64})
        self.wheels.write_text(json.dumps(changed))
        with self.assertRaises(AssertionError):
            self.verify()

    def test_identical_receipts_from_other_run_rejected(self):
        self.record['github_run_id'] = '122'
        self.native.write_text(json.dumps(self.record))
        self.wheels.write_bytes(self.native.read_bytes())
        with self.assertRaises(AssertionError):
            self.verify()

    def test_wrong_source_rejected(self):
        with self.assertRaises(AssertionError):
            verify_sources(self.native, self.wheels, 'd' * 40, 123)

    def test_missing_or_wrong_abi_rejected(self):
        directory = self.root / 'wheelhouse'
        directory.mkdir()
        for i in range(5):
            (directory / f'roastedduck-1.5.6.post2-cp31{i}-cp31{i}-macosx_11_0_arm64.whl').touch()
        verify_wheel_names(directory, '1.5.6.post2')
        (directory / 'roastedduck-1.5.6.post2-cp314-cp314-macosx_11_0_arm64.whl').unlink()
        with self.assertRaises(AssertionError):
            verify_wheel_names(directory, '1.5.6.post2')
        (directory / 'roastedduck-1.5.6.post2-cp314-cp314-manylinux_2_28_aarch64.whl').touch()
        with self.assertRaises(AssertionError):
            verify_wheel_names(directory, '1.5.6.post2')

    def test_required_jobs_come_from_immutable_source_workflow(self):
        with patch('observer_protocol.subprocess.check_output', return_value='jobs:\n  build:\n'):
            self.assertEqual(producer_job_names('a' * 40), REQUIRED_JOBS)
        with patch('observer_protocol.subprocess.check_output', return_value='jobs:\n  mac-native:\n  mac-package:\n'):
            self.assertEqual(producer_job_names('a' * 40), REQUIRED_JOBS | {'mac-native', 'mac-package'})

    def test_partial_split_policy_rejected(self):
        with patch('observer_protocol.subprocess.check_output', return_value='jobs:\n  mac-native:\n'):
            with self.assertRaises(AssertionError):
                producer_job_names('a' * 40)


if __name__ == '__main__':
    unittest.main()
