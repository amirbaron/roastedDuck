"""Frozen show() Optional[int] binding and body-cast boundaries, without normalization."""

from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import sys

import duckdb


class IndexLike:
    def __index__(self):
        return 8

    def __repr__(self):
        return "IndexLike(8)"


values = [("none", None), ("false", False), ("true", True), ("zero", 0), ("small", 8),
          ("negative", -1), ("overflow", 1 << 64), ("float", 8.0), ("str", "8"),
          ("bytes", b"8"), ("list", []), ("index", IndexLike())]
rows = {}
for option in ("max_width", "max_rows", "max_col_width"):
    for name, value in values:
        con = duckdb.connect()
        rel = con.sql("SELECT 42 AS answer")
        output = io.StringIO()
        try:
            with redirect_stdout(output):
                rel.show(**{option: value})
            rows[f"{option}/{name}"] = {"stdout": output.getvalue()}
        except Exception as error:
            rows[f"{option}/{name}"] = {"error": type(error).__name__, "message": str(error)}
        finally:
            con.close()

Path(sys.argv[1]).write_text(json.dumps(rows, indent=2, ensure_ascii=True) + "\n")
print(json.dumps({"cases": len(rows)}))
