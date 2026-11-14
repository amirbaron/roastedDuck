import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('render_homebrew', Path(__file__).parents[1] / 'render_homebrew.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class HomebrewFormulaTests(unittest.TestCase):
    def sums(self):
        return {f'roastedduck-1.5.6.post3-{p}.tar.gz': 'a' * 64
                for p in ('macos-arm64', 'linux-arm64', 'linux-amd64')}

    def test_requires_all_platform_hashes(self):
        sums = self.sums()
        sums.pop('roastedduck-1.5.6.post3-linux-arm64.tar.gz')
        with self.assertRaises(KeyError):
            module.render('1.5.6.post3', sums)

    def test_rejects_invalid_version_and_digest(self):
        with self.assertRaises(ValueError):
            module.render('1.5.6.post3";system("bad")', self.sums())
        sums = self.sums()
        sums['roastedduck-1.5.6.post3-macos-arm64.tar.gz'] = 'not-a-digest'
        with self.assertRaises(ValueError):
            module.render('1.5.6.post3', sums)

    def test_formula_includes_extension_tests_and_case_safe_alias(self):
        formula = module.render('1.5.6.post3', self.sums())
        self.assertEqual(formula.count('sha256 "' + 'a' * 64 + '"'), 3)
        self.assertIn('unless (bin/"roastedDuck").exist?', formula)
        self.assertIn('assert_match "HNSW_INDEX_SCAN"', formula)
        self.assertIn('.shellescape + " -batch', formula)
        self.assertIn('INSTALL fts;', formula)
        self.assertNotIn('"duckdb" =>', formula)
