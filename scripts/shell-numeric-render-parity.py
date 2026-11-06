"""Exact-byte numeric cell, readable-number and row-footer rendering journeys."""

import importlib.util
from pathlib import Path
import sys


spec = importlib.util.spec_from_file_location("shell_numeric_recorder",
                                            Path(__file__).with_name("shell-argv-parity.py"))
recorder = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = recorder
spec.loader.exec_module(recorder)
original_cases = recorder.cases


def numeric_cases(exhaustive):
    queries = (
        ("signed-cells", b"off", b"SELECT 1234567.89 a, -9876543.21 b, 0 c, NULL::DOUBLE d;"),
        ("readable-cells", b"all", b"SELECT 1234567 a, -9876543210 b, 12345 c;"),
        ("row-count", b"off", b"SELECT 42 AS value FROM range(12345);"),
        ("readable-footer", b"footer", b"SELECT 42 AS value FROM range(1234567);"),
    )
    for case in original_cases(exhaustive):
        if not case.name.endswith("/program-name"):
            continue
        label = case.name.split("/", 1)[0]
        value = case.argv0.removeprefix(b"duckdb-")
        for command in (b"decimal_sep", b"thousand_sep"):
            for name, large_mode, query in queries:
                yield recorder.Case(
                    f"{label}/{command.decode()}/{name}",
                    (b"-c", b".mode duckbox", b"-c", b".maxrows 4",
                     b"-c", b".large_number_rendering " + large_mode,
                     b"-c", b"." + command + b" " + recorder.meta_quoted(value),
                     b"-c", query),
                )


if __name__ == "__main__":
    recorder.cases = numeric_cases
    recorder.main()
