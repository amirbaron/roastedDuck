"""Terminal protocol checks for the differential harness (no engine required)."""

import importlib.util
import copy
import os
from pathlib import Path
import sys
import tempfile
import unittest


spec = importlib.util.spec_from_file_location("shell_diff", Path(__file__).with_name("shell-diff.py"))
shell_diff = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shell_diff)


class TerminalQueryTests(unittest.TestCase):
    def test_relative_work_directory_launches(self):
        with tempfile.TemporaryDirectory() as directory:
            work = os.path.relpath(Path(directory) / "case")
            result = shell_diff.run_pipe(sys.executable, work, {"args": ["-c", "print(42)"]})
            self.assertEqual(result["exit"], 0)
            self.assertEqual(result["stdout"], b"42\n")
            self.assertEqual(result["files"], {})

    def test_missing_startup_marker_is_a_readiness_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            result = shell_diff.run_pty(sys.executable, str(Path(directory) / "case"),
                                       {"args": ["-c", "print('different output')"],
                                        "keys": [], "startup_wait_for": "READY"})
            self.assertEqual(result["exit"], 0)
            self.assertEqual(result["wait_timeouts"], ["startup: READY"])

    def test_every_query_fragmentation(self):
        # PTY read boundaries are arbitrary, including one byte per read.
        for query, reply in [(b"\x1b[6n", b"\x1b[1;1R"),
                             (b"\x1b[c", b"\x1b[?1;2c"),
                             (b"\x1b[0c", b"\x1b[?1;2c")]:
            for cuts in range(1 << (len(query) - 1)):
                with self.subTest(query=query, cuts=cuts):
                    terminal = shell_diff.TerminalQueryResponder()
                    parts, start = [], 0
                    for index in range(1, len(query)):
                        if cuts & (1 << (index - 1)):
                            parts.append(query[start:index])
                            start = index
                    parts.append(query[start:])
                    self.assertEqual(b"".join(terminal.feed(part) for part in parts), reply)
                    self.assertEqual(terminal.feed(b"unrelated output"), b"")

    def test_background_probe_and_repeated_queries(self):
        terminal = shell_diff.TerminalQueryResponder()
        # DA1 is sufficient to finish the OSC 11 probe on a terminal that does
        # not support reporting its background color.
        self.assertEqual(terminal.feed(b"\x1b]11;?\x07\x1b[c"), b"\x1b[?1;2c")
        self.assertEqual(terminal.feed(b"\x1b[6n\x1b[6n\x1b[0c"),
                         b"\x1b[1;1R\x1b[1;1R\x1b[?1;2c")

    def test_output_and_unrecognized_controls_get_no_reply(self):
        terminal = shell_diff.TerminalQueryResponder()
        self.assertEqual(terminal.feed(b"text [c \x1b[31mred\x1b[0m \x1b[?1;2c"), b"")

    def test_linux_native_frames_use_existing_scoped_exception(self):
        case = {"known": "Implementation stack frames differ", "known_scope": "native_stack_trace"}
        prefix = b"INTERNAL Error: connection unavailable\n\nStack Trace:\n"
        reference = {"stdout": b"", "stderr": prefix + b"/bin/duckdb() [0x1234]\n"
                     b"/lib64/libc.so.6(__libc_start_main+0xe5) [0x5678]\n",
                     "exit": 1, "files": {}}
        rust = {**reference, "stderr": prefix + b"   0: connection\n"
                b"             at /workspace/engine.rs:10:2\n   1: main\n"}
        self.assertTrue(shell_diff.known_difference_allowed(case, reference, rust))
        for field, value in [("stdout", b"unexpected"), ("exit", 0), ("files", {"new": b"data"}),
                             ("stderr", rust["stderr"].replace(b"connection unavailable", b"wrong error")),
                             ("stderr", rust["stderr"] + b"unparsed trailing error\n")]:
            with self.subTest(field=field, value=value):
                changed = copy.deepcopy(rust)
                changed[field] = value
                self.assertFalse(shell_diff.known_difference_allowed(case, reference, changed))
        self.assertFalse(shell_diff.known_difference_allowed(
            case, {**reference, "stderr": prefix + b"not a native frame\n"}, rust))
        self.assertFalse(shell_diff.known_difference_allowed(
            case, {**reference, "stderr": prefix}, rust))

    def test_linux_native_frames_accept_duckdb_basename(self):
        case = {"known": "Implementation stack frames differ", "known_scope": "native_stack_trace"}
        prefix = b"INTERNAL Error: connection unavailable\n\nStack Trace:\n"
        reference = {"stdout": b"", "stderr": prefix + b"duckdb() [0x1234]\n"
                     b"/lib64/libc.so.6(__libc_start_main+0xe5) [0x5678]\n",
                     "exit": 1, "files": {}}
        rust = {**reference, "stderr": prefix + b"   0: connection\n"
                b"             at /workspace/engine.rs:10:2\n   1: main\n"}
        self.assertTrue(shell_diff.known_difference_allowed(case, reference, rust))
        for trace in [b"duckdb() missing address\n", b"unexpected() [0x1234]\n"]:
            self.assertFalse(shell_diff.known_difference_allowed(
                case, {**reference, "stderr": prefix + trace}, rust))


if __name__ == "__main__":
    unittest.main()
