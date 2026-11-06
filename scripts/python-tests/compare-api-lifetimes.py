"""Explicit descriptor/operator/destructor exports and live-result ownership."""
import gc
import json
from pathlib import Path
import sys

import duckdb

outcomes = {}


def capture(name, action):
    try:
        outcomes[name] = {"value": action()}
    except Exception as error:
        outcomes[name] = {"error": type(error).__name__, "message": str(error)}


for method in ["__div__", "__rdiv__"]:
    capture(method, lambda method=method: duckdb.sql("select 1").select(getattr(duckdb.ConstantExpression(8), method)(2)).fetchall())

for name in ["x", "absent", "_lookup_attribute"]:
    capture("relation_" + name, lambda name=name: duckdb.sql("select 42 x, 7 _lookup_attribute").__getattr__(name).fetchall())
for kind, name in [("INTEGER[]", "child"), ("MAP(VARCHAR,INTEGER)", "key"), ("STRUCT(x INTEGER)", "x"), ("STRUCT(_lookup_attribute INTEGER)", "_lookup_attribute"), ("INTEGER", "absent")]:
    capture(kind + "_" + name, lambda kind=kind, name=name: str(duckdb.sqltype(kind).__getattr__(name)))


def parent_gc():
    parent = duckdb.connect()
    child = parent.cursor()
    del parent
    gc.collect()
    return child.execute("select 7").fetchall()


capture("parent_gc_child_live", parent_gc)


def explicit_destructor():
    parent = duckdb.connect()
    child = parent.cursor()
    parent.__del__()
    parent.__del__()
    try:
        child.execute("select 8")
    except Exception as error:
        return [type(error).__name__, str(error)]
    return "unexpected_live_child"


capture("explicit_destructor", explicit_destructor)


def reader_lifetime():
    parent = duckdb.connect()
    reader = parent.sql("select i from range(5000)t(i)").to_arrow_reader(1024)
    parent.__del__()
    del parent
    gc.collect()
    table = reader.read_all()
    return [table.num_rows, table.column(0)[0].as_py(), table.column(0)[-1].as_py()]


capture("reader_after_destructor", reader_lifetime)
Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes}, indent=2) + "\n")
print(json.dumps({"cases": len(outcomes)}))
