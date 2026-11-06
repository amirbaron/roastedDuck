"""Native color errors and pager configuration, without launching a pager."""

import importlib.util
from pathlib import Path
import sys


spec = importlib.util.spec_from_file_location("shell_ui_recorder",
                                            Path(__file__).with_name("shell-argv-parity.py"))
recorder = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = recorder
spec.loader.exec_module(recorder)
original_cases = recorder.cases


def ui_cases(exhaustive):
    for case in original_cases(exhaustive):
        if not case.name.endswith("/program-name"):
            continue
        label = case.name.split("/", 1)[0]
        value = recorder.meta_quoted(case.argv0.removeprefix(b"duckdb-"))
        for name, command in (
            ("element", b".highlight_colors " + value + b" red"),
            ("color", b".highlight_colors keyword " + value),
            ("intensity", b".highlight_colors keyword red " + value),
            *((command.decode(), b"." + command + b" " + value)
              for command in (b"comment", b"constant", b"cont", b"cont_sel", b"error", b"keyword")),
        ):
            yield recorder.Case(f"{label}/{name}", (b"-c", command))
        yield recorder.Case(f"{label}/pager-command",
                            (b"-c", b".pager " + value, b"-c", b".pager"))
        yield recorder.Case(f"{label}/pager-row-threshold",
                            (b"-c", b".pager set_row_threshold " + value, b"-c", b".pager"))


if __name__ == "__main__":
    recorder.cases = ui_cases
    recorder.main()
