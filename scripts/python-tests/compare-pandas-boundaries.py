"""Nested parameter hints, object refeed, extension strings and categorical metadata."""
import json
from pathlib import Path
import sys

import duckdb
import numpy as np
import pandas as pd

outcomes = {}


def clean(value):
    if value is pd.NA:
        return {"pandas.NA": True}
    if isinstance(value, dict):
        return {str(key): clean(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return {type(value).__name__: [clean(item) for item in value]}
    if isinstance(value, np.ndarray):
        return {"ndarray": clean(value.tolist()), "dtype": str(value.dtype)}
    if isinstance(value, np.generic):
        return clean(value.item())
    return value


def capture(name, action):
    try:
        outcomes[name] = {"value": clean(action())}
    except Exception as error:
        outcomes[name] = {"error": type(error).__name__, "message": str(error)}


con = duckdb.connect()
for index, (value, target) in enumerate([
    (42, "SMALLINT"), ([], "INTEGER[]"), ([None], "INTEGER[]"),
    ([1, None, 2], "SMALLINT[]"), ([1, 2], "INTEGER[3]"),
    ([[{"a": 5}], None], "STRUCT(a INTEGER)[]"),
    ([[1, 2]], "INTEGER"), ([None, []], "INTEGER[][]"),
    ({"a": [1, None]}, "STRUCT(a INTEGER[])"),
    ([{"a": 1}, {"a": None}], "STRUCT(a INTEGER)[]"),
]):
    for type_form in ["string", "native"]:
        def run(value=value, target=target, type_form=type_form):
            hint = target if type_form == "string" else duckdb.sqltype(target)
            parameter = duckdb.Value(value, hint)
            return con.sql("select ?, typeof(?)", params=[parameter, parameter]).fetchall()
        capture(f"parameter_{index}_{type_form}", run)

for dtype in ["string[python]", "string[pyarrow]", "string[pyarrow_numpy]"]:
    def strings(dtype=dtype):
        frame = pd.DataFrame({"x": pd.Series(["雪", None, "a"], dtype=dtype)})
        return con.from_df(frame).fetchall()
    capture(dtype, strings)

for method in ["execute", "sql", "query"]:
    for query in ["select NULL::ENUM('b','a') e where false", "select 'b'::ENUM('b','a') e"]:
        def categorical(method=method, query=query):
            frame = getattr(con, method)(query).df()
            return [frame.e.tolist(), frame.e.cat.categories.tolist(), frame.e.cat.ordered]
        capture(method + "_" + query, categorical)

for data in [[[ {1: 10, 2: 20}, {1: 10, 3: 30} ]], [[{"a": 1}, {"b": 2}]]]:
    def refeed(data=data):
        frame = pd.DataFrame({"x": [np.array(data[0], dtype=object)]})
        return con.from_df(frame).fetchall()
    capture("refeed_" + repr(data), refeed)

Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes}, indent=2, ensure_ascii=False) + "\n")
print(json.dumps({"cases": len(outcomes)}))
