"""Actual optional-library tensors: values, dtypes, errors and source lifetime."""
import gc
import json
from pathlib import Path
import sys

import duckdb
import numpy as np

backend = sys.argv[1]
library = __import__("torch" if backend == "torch" else "tensorflow")
outcomes = {}

def clean(value):
    if isinstance(value, bytes): return {"bytes": value.hex()}
    if isinstance(value, list): return [clean(item) for item in value]
    if isinstance(value, tuple): return [clean(item) for item in value]
    if isinstance(value, dict): return {key:clean(item) for key,item in value.items()}
    return value


def summarize(result):
    if result is None:
        return None
    return {name: {"dtype": str(value.dtype), "shape": list(value.shape),
                   "values": value.numpy().tolist()} for name, value in result.items()}


def capture(name, action):
    try:
        outcomes[name] = {"value": clean(action())}
    except Exception as error:
        outcomes[name] = {"error": type(error).__name__, "message": str(error)}


types = ["BOOLEAN", "TINYINT", "SMALLINT", "INTEGER", "BIGINT", "HUGEINT", "UTINYINT", "USMALLINT", "UINTEGER", "UBIGINT", "UHUGEINT", "FLOAT", "DOUBLE", "DECIMAL(10,2)"]
for kind in types:
    for owner in ["connection", "relation", "module"]:
        def numeric(kind=kind, owner=owner):
            con = duckdb.connect()
            query = f"select i::{kind} x from range(3)t(i)"
            if owner == "module":
                con.execute(query)
                result = getattr(duckdb, backend)(connection=con)
            elif owner == "connection":
                result = getattr(con.execute(query), backend)()
            else:
                result = getattr(con.sql(query), backend)()
            con.close()
            del con
            gc.collect()
            return summarize(result)
        capture(kind + "_" + owner, numeric)

for label, query in [
    ("empty", "select i::integer x from range(0)t(i)"),
    ("duplicates", "select 1 a,2 a"),
    ("text", "select 'snow' x"),
    ("timestamp", "select timestamp '2000-01-01' x"),
    ("list", "select [1,2] x"),
    ("bignum", "select 123::bignum x"),
]:
    capture(label, lambda query=query: summarize(getattr(duckdb.sql(query), backend)()))


def repeat():
    con = duckdb.connect()
    con.execute("select 1 x")
    first = summarize(getattr(con, backend)())
    try:
        again = summarize(getattr(con, backend)())
    except Exception as error:
        again = {"error": type(error).__name__, "message": str(error)}
    return [first, again]


capture("repeat", repeat)
capture("no_result", lambda: summarize(getattr(duckdb.connect(), backend)()))


def lifetime():
    con = duckdb.connect()
    result = getattr(con.execute("select i::bigint x from range(5000)t(i)"), backend)()
    con.execute("select 99")
    con.close()
    del con
    gc.collect()
    array = result["x"].numpy()
    return [len(array), int(array.sum()), int(array[0]), int(array[-1])]


capture("lifetime", lifetime)
Path(sys.argv[2]).write_text(json.dumps({"backend": backend, "version": library.__version__, "outcomes": outcomes}, indent=2) + "\n")
print(json.dumps({"backend": backend, "cases": len(outcomes)}))
