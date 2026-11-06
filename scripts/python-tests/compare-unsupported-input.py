"""Capture unsupported replacement-scan contracts against the original client."""
import json
import sys
from pathlib import Path

import duckdb
import pandas as pd


class InterchangeFrame:
    def __init__(self):
        self.frame = pd.DataFrame({"i": [1, 2, 3]})

    def __dataframe__(self, *args, **kwargs):
        return self.frame.__dataframe__(*args, **kwargs)


class UnsupportedDescriptor:
    def __init__(self):
        self.accesses = 0

    @property
    def __dataframe__(self):
        self.accesses += 1
        raise RuntimeError("Unsupported protocol descriptor must not run")


def capture(incoming):
    with duckdb.connect() as connection:
        try:
            rows = connection.execute("SELECT sum(i) FROM incoming").fetchall()
            return {"rows": rows}
        except Exception as error:
            return {"error_type": type(error).__name__, "error": str(error)}


records = []
for incoming in [InterchangeFrame(), UnsupportedDescriptor(), object()]:
    result = capture(incoming)
    assert result.get("error_type") == "InvalidInputException", result
    if isinstance(incoming, UnsupportedDescriptor):
        assert incoming.accesses == 0
        result["unsupported_descriptor_accesses"] = incoming.accesses
    records.append(result)
try:
    duckdb.enum_type("e", None, ["a", "b"])
except Exception as error:
    records.append({"error_type": type(error).__name__, "error": str(error)})
else:
    raise AssertionError("Reference enum constructor contract changed")
Path(sys.argv[1]).write_text(json.dumps({"records": records}, indent=2) + "\n")
