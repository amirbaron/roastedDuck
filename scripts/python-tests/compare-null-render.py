"""Real Python NULL configuration, rendering/cache and fetch behavior.

Run separately in each installed engine environment, retaining exact text/errors.
No pointer/version/decoder-byte normalization is performed here.
"""

from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import sys

import duckdb


values = [
    ("default", None), ("empty", ""), ("ascii", "missing"),
    ("unicode", "雪🦆e\u0301"), ("tab", "a\tb"), ("newline", "a\nb"),
    ("nul", "a\0b"), ("ansi", "\x1b[31mNULL\x1b[0m"),
    ("bytes-ascii", b"missing"), ("bytes-utf8", "雪🦆".encode()),
    ("bytearray-ascii", bytearray(b"missing")), ("bytes-invalid", b"\xff"),
    ("bytearray-invalid", bytearray(b"\xff")),
    ("int", 42), ("bool", False), ("list", ["missing"]),
]
outcomes = {}
for name, value in values:
    for mode in ("default", "integer-columns"):
        key = f"{name}/{mode}"
        con = duckdb.connect()
        output = io.StringIO()
        try:
            rel = con.sql("SELECT NULL::VARCHAR AS value, 42 AS answer")
            before = str(rel)
            options = {"null_value": value}
            if mode == "integer-columns":
                options["render_mode"] = 1
            with redirect_stdout(output):
                rel.show(**options)
            outcomes[key] = {"stdout": output.getvalue(), "before": before,
                             "after": str(rel), "fetch": rel.fetchall()}
        except Exception as error:
            outcomes[key] = {"stdout": output.getvalue(), "error": type(error).__name__,
                             "message": str(error)}
        finally:
            con.close()

Path(sys.argv[1]).write_text(json.dumps({"version": duckdb.__version__, "outcomes": outcomes},
                                     indent=2, ensure_ascii=True) + "\n")
print(json.dumps({"cases": len(outcomes)}))
