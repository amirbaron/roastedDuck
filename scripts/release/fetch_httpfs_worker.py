#!/usr/bin/env python3
"""Use the official C++ SDK's INSTALL/LOAD path in an isolated process."""
import hashlib
import json
from pathlib import Path
import sys

import duckdb
import _duckdb

assert duckdb.__version__ == "1.5.6" and not getattr(_duckdb, "__rust_engine__", False)
directory, platform, expected_hash, report = sys.argv[1:]
directory = Path(directory).resolve()
directory.mkdir(parents=True, exist_ok=True)
api = duckdb.connect(config={"extension_directory": str(directory / "extensions"),
                            "secret_directory": str(directory / "secrets"),
                            "allow_unsigned_extensions": False, "allow_community_extensions": False})
queries = []


def query(sql):
    rows = api.execute(sql).fetchall()
    queries.append({"sql": sql, "rows": rows})
    return rows


try:
    assert query("PRAGMA platform") == [(platform,)]
    for setting in ["allow_unsigned_extensions", "allow_community_extensions", "force_mbedtls_unsafe"]:
        assert query("SELECT current_setting('" + setting + "')::VARCHAR") == [("false",)]
    query("INSTALL httpfs FROM 'https://extensions.duckdb.org'")
    files = list((directory / "extensions").rglob("httpfs.duckdb_extension"))
    assert len(files) == 1
    native = files[0].resolve()
    actual = hashlib.sha256(native.read_bytes()).hexdigest()
    assert actual == expected_hash, (native, actual, expected_hash)
    query("LOAD '" + str(native).replace("'", "''") + "'")
    assert query("SELECT loaded FROM duckdb_extensions() WHERE extension_name='httpfs'") == [(True,)]
    Path(report).write_text(json.dumps({"passed": True, "library": str(Path(_duckdb.__file__).resolve()),
                                       "platform": platform, "httpfs": str(native), "sha256": actual,
                                       "queries": queries}, indent=2) + "\n")
finally:
    api.close()
