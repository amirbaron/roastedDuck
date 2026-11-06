"""Frozen module/connection type constructor contracts and catalog/lifetime journeys."""
import json
import os
from pathlib import Path
import re
import sys

import duckdb

outcomes = {}
raw = {}


def capture(name, action):
    print("case", name, flush=True)
    try:
        outcomes[name] = {"value": action()}
    except Exception as error:
        message = str(error)
        raw[name] = {"error": type(error).__name__, "message": message}
        if type(error).__name__ == "InternalException":
            message = message.split("\n\nStack Trace:\n", 1)[0].rstrip()
        message = re.sub(r"(_duckdb.DuckDBPyConnection object at )0x[0-9a-f]+", r"\1<address>", message)
        outcomes[name] = {"error": type(error).__name__, "message": message}
    Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes, "raw_errors": raw}, indent=2, ensure_ascii=False) + "\n")


cases = [
    ("array_type", (int, 3), {}), ("array_type", (), {"type": "INTEGER", "size": 2}),
    ("array_type", (int, 0), {}), ("array_type", (int, 2**32), {}),
    ("array_type", (int, -1), {}), ("array_type", (None, 2), {}),
    ("array_type", (int, 1.5), {}), ("array_type", (int, 2), {"size": 3}),
    ("decimal_type", (10, 2), {}), ("decimal_type", (), {"width": 8, "scale": 3}),
    ("decimal_type", (0, 0), {}), ("decimal_type", (39, 0), {}),
    ("decimal_type", (-1, 2), {}), ("decimal_type", (10, 11), {}),
    ("decimal_type", (256, 0), {}), ("decimal_type", (1.5, 0), {}),
    ("decimal_type", (10,), {}), ("decimal_type", (10, 2, 3), {}),
    ("list_type", (int,), {}), ("list_type", (), {"type": "INTEGER"}),
    ("list_type", (None,), {}), ("list_type", (42,), {}),
    ("map_type", (str, int), {}), ("map_type", (), {"key": str, "value": int}),
    ("map_type", (None, int), {}), ("map_type", (str, None), {}),
    ("struct_type", ({"a": int, "雪": str},), {}),
    ("struct_type", ({1: int},), {}), ("struct_type", ([int, str],), {}),
    ("struct_type", ({},), {}), ("struct_type", ([],), {}),
    ("struct_type", ((int, str),), {}), ("struct_type", (None,), {}),
    ("struct_type", ([42],), {}), ("struct_type", ({"a": 42},), {}),
    ("row_type", ({"a": int},), {}), ("row_type", ([int, str],), {}),
    ("union_type", ({"a": int, "b": str},), {}),
    ("union_type", ([int, str],), {}), ("union_type", ({},), {}),
    ("union_type", (None,), {}), ("union_type", ((),), {}),
    ("string_type", (), {}), ("string_type", ("nocase",), {}),
    ("string_type", (), {"collation": "nocase"}), ("string_type", (None,), {}),
    ("enum_type", ("test_enum", str, ["a", "b"]), {}),
    ("enum_type", ("test_enum", str, ("a",)), {}),
    ("enum_type", ("test_enum", None, ["a"]), {}),
]
for name in ["type", "dtype", "sqltype"]:
    cases.extend([(name, ("INTEGER",), {}), (name, (), {"type_str": "INTEGER[]"}),
                  (name, (None,), {}), (name, (42,), {}), (name, ("not_a_type",), {})])

cases.extend([
    ("dtype", (b"INTEGER",), {}), ("dtype", (bytearray(b"INTEGER"),), {}),
    ("dtype", (b"\xff",), {}), ("string_type", (b"nocase",), {}),
    ("enum_type", (b"name", str, ["x"]), {}), ("enum_type", ("x", None, None), {}),
    ("list_type", ("not_a_type",), {}), ("array_type", (42, 2), {}),
    ("map_type", (42, int), {}), ("struct_type", (["not_a_type"],), {}),
])
for owner_kind in ["module", "connection", "closed"]:
    for index, (method, args, kwargs) in enumerate(cases):
        def run(method=method, args=args, kwargs=kwargs, owner_kind=owner_kind):
            con = duckdb.connect()
            if owner_kind == "closed":
                con.close()
            owner = duckdb if owner_kind == "module" else con
            return str(getattr(owner, method)(*args, **kwargs))
        capture(f"{owner_kind}_{index}_{method}", run)


def catalog():
    con = duckdb.connect()
    con.execute("begin;create type local_type as enum ('warm','cold')")
    kinds = [str(getattr(con, name)("local_type[]")) for name in ["type", "dtype", "sqltype"]]
    kinds.append(str(duckdb.sqltype("local_type", connection=con)))
    con.execute("rollback")
    try:
        con.dtype("local_type")
    except duckdb.CatalogException:
        kinds.append("rollback_removed_type")
    return kinds


capture("catalog_transaction", catalog)


def active_stream():
    con = duckdb.connect()
    reader = con.sql("select i from range(5000)t(i)").to_arrow_reader(1024)
    first = reader.read_next_batch().num_rows
    kind = str(con.dtype("INTEGER"))
    rest = reader.read_all().num_rows
    return [first, kind, rest]


capture("active_stream", active_stream)


def default_catalog():
    duckdb.execute("create type default_type_probe as enum ('x','y')")
    return [str(duckdb.dtype("default_type_probe")),
            str(duckdb.sqltypes.DuckDBPyType("default_type_probe")),
            str(duckdb.array_type("default_type_probe", 2))]


capture("default_catalog", default_catalog)


def reentrant():
    con = duckdb.connect()
    con.execute("create type callback_type_probe as enum ('a','b')")
    seen = []

    def callback(value):
        seen.append(str(con.dtype("callback_type_probe")))
        return value

    con.create_function("type_probe", callback, [int], int, side_effects=True)
    result = con.execute("select type_probe(i) from range(3)t(i)").fetchall()
    return [seen, result]


if not os.environ.get("TYPE_API_SKIP_REENTRANT"):
    capture("reentrant_lookup", reentrant)
Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes, "raw_errors": raw}, indent=2, ensure_ascii=False) + "\n")
print(json.dumps({"cases": len(outcomes)}))
