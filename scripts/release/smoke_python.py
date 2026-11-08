#!/usr/bin/env python3
"""Exercise an installed preview wheel, including version identity and persistence."""
import importlib.metadata
import json
from pathlib import Path
import tempfile

import duckdb

assert importlib.metadata.version("roastedduck") == "0.1.0a1"
assert duckdb.__version__ == duckdb.__duckdb_version__ == "1.5.6"
assert duckdb.version() == "0.1.0a1 (with duckdb 1.5.6)"
try:
    importlib.metadata.version("duckdb")
except importlib.metadata.PackageNotFoundError:
    pass
else:
    raise AssertionError("Official duckdb distribution must not be co-installed")

assert duckdb.sql("SELECT sum(i) FROM range(10) t(i)").fetchone() == (45,)
with tempfile.TemporaryDirectory() as temp:
    database = str(Path(temp) / "smoke.duckdb")
    parquet = str(Path(temp) / "smoke.parquet")
    with duckdb.connect(database) as con:
        con.execute("CREATE TABLE t(i INTEGER, s VARCHAR)")
        con.execute("INSERT INTO t VALUES (?, ?)", [42, "roastedDuck 🦆"])
        con.execute("BEGIN TRANSACTION")
        con.execute("INSERT INTO t VALUES (99, 'rolled back')")
        con.execute("ROLLBACK")
        con.execute("COPY t TO ? (FORMAT PARQUET)", [parquet])
    with duckdb.connect(database) as con:
        assert con.execute("SELECT * FROM t").fetchall() == [(42, "roastedDuck 🦆")]
        assert con.execute("SELECT * FROM read_parquet(?)", [parquet]).fetchall() == [(42, "roastedDuck 🦆")]
print(json.dumps({"status": "passed", "distribution": "roastedduck", "version": duckdb.__version__,
                  "engine_version": duckdb.__duckdb_version__, "extension": duckdb.__file__,
                  "checks": ["identity", "aggregate", "parameters", "unicode", "rollback", "reopen", "parquet"]}))
