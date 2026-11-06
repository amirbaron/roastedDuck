"""Catalog filters and import arguments through actual byte-valued dot commands.

Uses the same isolated exact-byte recorder as the argv/path probes. The engines,
SQL, errors and filesystem effects are not mocked or normalized.
"""

import importlib.util
from pathlib import Path
import sys


spec = importlib.util.spec_from_file_location("shell_catalog_recorder",
                                            Path(__file__).with_name("shell-argv-parity.py"))
recorder = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = recorder
spec.loader.exec_module(recorder)
original_cases = recorder.cases


def catalog_cases(exhaustive):
    setup = b"CREATE TABLE alpha(i INTEGER); INSERT INTO alpha VALUES(42); CREATE INDEX idx_alpha ON alpha(i);"
    for case in original_cases(exhaustive):
        if not case.name.endswith("/program-name"):
            continue
        label = case.name.split("/", 1)[0]
        value = case.argv0.removeprefix(b"duckdb-")
        for name, command in [("schema", b".schema"), ("schema-debug", b".schema --debug"),
                              ("schema-indent", b".schema --indent"), ("tables", b".tables"),
                              ("indexes", b".indexes"), ("dump", b".dump")]:
            yield recorder.Case(f"{label}/{name}",
                                (b"-c", setup, b"-c", command + b" " + recorder.meta_quoted(value)))
        filename = b"missing-" + value + b".csv"
        yield recorder.Case(f"{label}/import-missing-path",
                            (b"-c", b".import " + recorder.meta_quoted(filename) + b" alpha"))
        table = b"native-" + value
        yield recorder.Case(f"{label}/import-table",
                            (b"-c", b".import input.csv " + recorder.meta_quoted(table),
                             b"-c", b"SELECT count(*) AS n FROM " + recorder.quoted(table, b'"') + b";"),
                            files=((b"input.csv", b"i\n42\n"),))


if __name__ == "__main__":
    recorder.cases = catalog_cases
    recorder.main()
