#!/usr/bin/env python3
"""Exercise the installed Rust wheel's encrypted storage and Parquet paths."""
import decimal
import json
from pathlib import Path
import tempfile

import duckdb
from duckdb import _duckdb

assert _duckdb.__rust_engine__ is True
expected = [(7, "שלום雪", b"\x00\xff", decimal.Decimal("123.450"), [1, None, 3])]
profiles = []
with tempfile.TemporaryDirectory(prefix="roastedduck-encryption-") as temporary:
    root = Path(temporary)
    for cipher in ["GCM", "CTR"]:
        for version in ["v0", "v1"]:
            path = str(root / f"{cipher}-{version}.duckdb").replace("'", "''")
            options = f"ENCRYPTION_CIPHER '{cipher}', debug_encryption_version '{version}'"
            with duckdb.connect() as connection:
                connection.execute(f"ATTACH '{path}' AS sealed (ENCRYPTION_KEY 'synthetic-preview-test-key', {options})")
                connection.execute("CREATE TABLE sealed.samples(i INTEGER, s VARCHAR, b BLOB, d DECIMAL(18,3), xs INTEGER[])")
                connection.execute("INSERT INTO sealed.samples VALUES (?, ?, ?, ?, ?)", list(expected[0]))
                connection.execute("CHECKPOINT sealed")
            with duckdb.connect() as connection:
                try:
                    connection.execute(f"ATTACH '{path}' AS bad (ENCRYPTION_KEY 'wrong-key', {options})")
                except duckdb.Error as error:
                    assert "Wrong encryption key used" in str(error)
                else:
                    raise AssertionError("Incorrect encryption key was accepted")
                connection.execute(f"ATTACH '{path}' AS sealed (ENCRYPTION_KEY 'synthetic-preview-test-key', {options})")
                assert connection.execute("FROM sealed.samples").fetchall() == expected
            profiles.append({"cipher": cipher, "version": version, "checkpoint_reopen": True, "wrong_key_rejected": True})
    with duckdb.connect() as connection:
        connection.execute("CREATE TABLE sample AS SELECT i, '雪' || i::VARCHAR AS s FROM range(2048) t(i)")
        for size in [16, 24, 32]:
            key = "0123456789abcdef0123456789abcdef"[:size]
            connection.execute(f"PRAGMA add_parquet_key('right', '{key}')")
            connection.execute("PRAGMA add_parquet_key('wrong', 'fedcba9876543210')")
            file = str(root / f"encrypted-{size}.parquet").replace("'", "''")
            connection.execute(f"COPY sample TO '{file}' (FORMAT PARQUET, ENCRYPTION_CONFIG {{footer_key:'right'}})")
            count = connection.execute(f"SELECT count(*) FROM ((FROM sample EXCEPT ALL FROM read_parquet('{file}', encryption_config={{footer_key:'right'}})) UNION ALL (FROM read_parquet('{file}', encryption_config={{footer_key:'right'}}) EXCEPT ALL FROM sample))").fetchone()[0]
            assert count == 0
            try:
                connection.execute(f"FROM read_parquet('{file}', encryption_config={{footer_key:'wrong'}})").fetchall()
            except duckdb.Error:
                pass
            else:
                raise AssertionError("Incorrect Parquet key was accepted")
print(json.dumps({"passed": True, "database_profiles": profiles, "parquet_key_bits": [128, 192, 256]}))
