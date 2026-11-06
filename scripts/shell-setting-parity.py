"""Exact-byte boolean/numeric/render-setting dot-command comparisons."""

import importlib.util
from pathlib import Path
import sys


spec = importlib.util.spec_from_file_location("shell_setting_recorder",
                                            Path(__file__).with_name("shell-argv-parity.py"))
recorder = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = recorder
spec.loader.exec_module(recorder)
original_cases = recorder.cases


def setting_cases(exhaustive):
    for case in original_cases(exhaustive):
        if not case.name.endswith("/program-name"):
            continue
        label = case.name.split("/", 1)[0]
        value = case.argv0.removeprefix(b"duckdb-")
        for command in (b"bail", b"binary", b"changes", b"echo", b"headers", b"timer",
                        b"highlight", b"highlight_errors", b"highlight_results",
                        b"large_number_rendering"):
            yield recorder.Case(f"{label}/{command.decode()}",
                                (b"-c", b"." + command + b" " + recorder.meta_quoted(value),
                                 b"-c", b"SELECT 42 AS answer;"))
        for command in (b"decimal_sep", b"thousand_sep"):
            yield recorder.Case(f"{label}/{command.decode()}",
                                (b"-c", b"." + command + b" " + recorder.meta_quoted(value),
                                 b"-c", b"SELECT 12345.6789 AS value;"))
        for command in (b"maxrows", b"maxwidth", b"width"):
            yield recorder.Case(f"{label}/{command.decode()}",
                                (b"-c", b"." + command + b" " + recorder.meta_quoted(value),
                                 b"-c", b"SELECT 42 AS answer;"))
        for command in (b"mode", b"startup_text", b"highlight_mode", b"help"):
            yield recorder.Case(f"{label}/{command.decode()}",
                                (b"-c", b"." + command + b" " + recorder.meta_quoted(value)))
        table = b"native-" + value
        yield recorder.Case(f"{label}/insert-table-name",
                            (b"-c", b".mode insert " + recorder.meta_quoted(table),
                             b"-c", b"SELECT 42 AS answer;"))


if __name__ == "__main__":
    recorder.cases = setting_cases
    recorder.main()
