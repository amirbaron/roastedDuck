#!/usr/bin/env python3
"""Exercise an installed preview wheel, including version identity and persistence."""
import importlib.metadata
import argparse
import functools
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading

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
# The other bundled extensions, offline: ICU collation and time zones, the HTTP
# filesystem against a loopback server, the S3 and Azure secret types and
# providers, and the PostgreSQL scanner's functions.
with tempfile.TemporaryDirectory() as temp, duckdb.connect(":memory:") as con:
    (Path(temp) / "rows.csv").write_text("i\n" + "".join(f"{i}\n" for i in range(100)))

    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *unused):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Quiet, directory=temp))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    saved = {name: os.environ.get(name) for name in ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"]}
    os.environ.update(AWS_ACCESS_KEY_ID="smoke", AWS_SECRET_ACCESS_KEY="smoke")
    try:
        for name in ["icu", "httpfs", "aws", "azure", "postgres_scanner"]:
            con.load_extension(name)
        assert con.execute("SELECT 'ö' < 'z' COLLATE de, (TIMESTAMPTZ '2024-01-01 00:00:00+00' AT TIME ZONE 'Asia/Tokyo')::VARCHAR"
                           ).fetchone() == (True, "2024-01-01 09:00:00")
        assert con.execute("SELECT sum(i) FROM read_csv(?)", [f"http://127.0.0.1:{server.server_port}/rows.csv"]).fetchone() == (4950,)
        con.execute("CREATE SECRET smoke_s3 (TYPE s3, KEY_ID 'smoke', SECRET 'smoke', REGION 'eu-west-1')")
        con.execute("CREATE SECRET smoke_chain (TYPE s3, PROVIDER credential_chain, CHAIN 'env', REGION 'eu-west-1')")
        con.execute("CREATE SECRET smoke_azure (TYPE azure, CONNECTION_STRING 'DefaultEndpointsProtocol=http;"
                    "AccountName=devstoreaccount1;AccountKey=c21va2U=;BlobEndpoint=http://127.0.0.1:1/devstoreaccount1;')")
        assert con.execute("SELECT name, type, provider FROM duckdb_secrets() WHERE name LIKE 'smoke_%' ORDER BY name").fetchall() == [
            ("smoke_azure", "azure", "config"), ("smoke_chain", "s3", "credential_chain"), ("smoke_s3", "s3", "config")]
        assert con.execute("SELECT count(*) > 0 FROM duckdb_functions() WHERE function_name = 'postgres_scan'").fetchone() == (True,)
        assert con.execute("SELECT count(*) FROM duckdb_extensions() WHERE loaded AND extension_name IN "
                           "('aws', 'azure', 'httpfs', 'icu', 'postgres_scanner')").fetchone() == (5,)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
        for name, value in saved.items():
            if value is None:
                del os.environ[name]
            else:
                os.environ[name] = value
print(json.dumps({"status": "passed", "distribution": "roastedduck", "version": importlib.metadata.version("roastedduck"),
                  "engine_version": duckdb.__duckdb_version__, "extension": duckdb.__file__,
                  "checks": ["identity", "aggregate", "parameters", "unicode", "rollback", "reopen", "parquet", "bundled_fts", "bundled_vss", "offline_install",
                             "bundled_icu", "bundled_httpfs", "bundled_aws", "bundled_azure", "bundled_postgres_scanner"]}))
