#!/usr/bin/env python3
"""Exercise an installed preview wheel, including version identity and persistence."""
import importlib.metadata
import argparse
import json
from pathlib import Path
import tempfile

import duckdb

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--release", type=Path, default=Path(__file__).resolve().parents[2] / "release.json")
release = json.loads(parser.parse_args().release.read_text())
assert importlib.metadata.version("roastedduck") == release["version"]
assert duckdb.__version__ == duckdb.__duckdb_version__ == release["compatibility_version"]
assert duckdb.version() == f'{release["version"]} (with duckdb {release["compatibility_version"]})'
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
# Exercise the bundled implementations through the installed Python binding.
with duckdb.connect(":memory:") as con:
    con.execute("SET custom_extension_repository='http://127.0.0.1:1'")
    con.install_extension("fts")
    con.install_extension("vss")
    con.load_extension("fts")
    con.load_extension("vss")
    assert con.execute("SELECT stem('connections','english')").fetchone() == ("connect",)
    con.execute("CREATE TABLE docs(id INTEGER, body VARCHAR)")
    con.execute("INSERT INTO docs VALUES (1,'red fox'),(2,'blue bird')")
    con.execute("PRAGMA create_fts_index('docs','id','body')")
    assert con.execute("SELECT id FROM docs WHERE fts_main_docs.match_bm25(id,'fox') IS NOT NULL").fetchall() == [(1,)]
    con.execute("CREATE TABLE vectors(id INTEGER,v FLOAT[3])")
    con.execute("INSERT INTO vectors VALUES (1,[1,0,0]),(2,[0,1,0])")
    con.execute("CREATE INDEX h ON vectors USING HNSW(v)")
    assert con.execute("SELECT id FROM vectors ORDER BY array_distance(v,[1,0,0]::FLOAT[3]) LIMIT 1").fetchone() == (1,)
print(json.dumps({"status": "passed", "distribution": "roastedduck", "version": importlib.metadata.version("roastedduck"),
                  "engine_version": duckdb.__duckdb_version__, "extension": duckdb.__file__,
                  "checks": ["identity", "aggregate", "parameters", "unicode", "rollback", "reopen", "parquet", "bundled_fts", "bundled_vss", "offline_install"]}))
