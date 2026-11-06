"""Exact-byte prompt/progress parsing and custom UI command comparisons."""

import importlib.util
from pathlib import Path
import sys


spec = importlib.util.spec_from_file_location("shell_prompt_recorder",
                                            Path(__file__).with_name("shell-argv-parity.py"))
recorder = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = recorder
spec.loader.exec_module(recorder)
original_cases = recorder.cases


def prompt_cases(exhaustive):
    for case in original_cases(exhaustive):
        if not case.name.endswith("/program-name"):
            continue
        label = case.name.split("/", 1)[0]
        value = case.argv0.removeprefix(b"duckdb-")
        components = [("bracket", b"{" + value + b":x}"),
                      ("setting", b"{setting:" + value + b"}"),
                      ("color", b"{color:" + value + b"}"),
                      ("highlight", b"{highlight_element:" + value + b"}"),
                      ("unterminated", value + b"{"),
                      ("escape", value + b"\\")]
        for command in (b"prompt", b"progress_bar --add"):
            for name, component in components:
                yield recorder.Case(f"{label}/{command.decode()}/{name}",
                                    (b"-c", b"." + command + b" " + recorder.meta_quoted(component)))
        for name, option in [("align", b"align"), ("content-align", b"content_align"),
                             ("hide-pattern", b"hide_if_contains"), ("min-size", b"min_size")]:
            component = b"{" + option + b":" + value + b"}value"
            yield recorder.Case(f"{label}/progress/{name}",
                                (b"-c", b".progress_bar --add " + recorder.meta_quoted(component)))
        yield recorder.Case(f"{label}/prompt/max-length",
                            (b"-c", b".prompt " + recorder.meta_quoted(b"{max_length:" + value + b"}value")))
        procedure = recorder.quoted(b"missing-native-ui-" + value, b'"') + b"()"
        yield recorder.Case(f"{label}/ui-command",
                            (b"-cmd", b".ui_command " + recorder.meta_quoted(procedure), b"-ui"))
    for name, number in [("negative", b"-1"), ("plus", b"+1"), ("space", b" 2"),
                         ("suffix", b"12tail"), ("overflow", b"18446744073709551616"),
                         ("unicode-space", "\u00a01".encode())]:
        for command, option in [(b"prompt", b"max_length"), (b"progress_bar --add", b"min_size")]:
            yield recorder.Case(f"numeric-{name}/{option.decode()}",
                                (b"-c", b"." + command + b" " +
                                 recorder.meta_quoted(b"{" + option + b":" + number + b"}value")))


if __name__ == "__main__":
    recorder.cases = prompt_cases
    recorder.main()
