"""Native byte-path dot commands using the argv probe's exact-byte recorder.

This composes test cases only; neither engine nor its outcomes are replaced.
Use fresh external --work/--out paths and run serially with other engine workloads.
"""

import importlib.util
from pathlib import Path
import sys


source = Path(__file__).with_name("shell-argv-parity.py")
spec = importlib.util.spec_from_file_location("shell_native_path_recorder", source)
recorder = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = recorder
spec.loader.exec_module(recorder)
original_cases = recorder.cases


def dot_path_cases(exhaustive):
    # Reuse precisely the same payload classes, including the optional byte matrix.
    for case in original_cases(exhaustive):
        if not case.name.endswith("/program-name"):
            continue
        label = case.name.split("/", 1)[0]
        payload = case.argv0.removeprefix(b"duckdb-")
        for command, suffix in [(b"open", b".db"), (b"output", b".txt"),
                                (b"once", b".txt"), (b"log", b".txt")]:
            filename = b"native-" + payload + suffix
            line = b"." + command + b" " + recorder.meta_quoted(filename)
            yield recorder.Case(f"{label}/dot-{command.decode()}",
                                (b"-c", line, b"-c", b"SELECT 42 AS answer;"))
        for command, suffix in [(b"read", b".sql"), (b"cd", b"-directory")]:
            filename = b"missing-" + payload + suffix
            line = b"." + command + b" " + recorder.meta_quoted(filename)
            yield recorder.Case(f"{label}/dot-{command.decode()}-missing", (b"-c", line))
        filename = b"native-" + payload + b".sql"
        yield recorder.Case(f"{label}/dot-read-existing",
                            (b"-c", b".read " + recorder.meta_quoted(filename)),
                            files=((filename, b"SELECT 42 AS from_file;\n"),))
        # The path returned by SQL must also stay native through .open's --sql path.
        sql = b"SELECT " + recorder.quoted(b"native-" + payload + b".db", b"'")
        yield recorder.Case(f"{label}/dot-open-sql",
                            (b"-c", b".open --sql " + recorder.meta_quoted(sql),
                             b"-c", b"SELECT 43 AS after_open;"))


if __name__ == "__main__":
    recorder.cases = dot_path_cases
    recorder.main()
