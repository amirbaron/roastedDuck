"""Real Python buffer input and GEOMETRY host-output journeys against each engine."""
import array
import gc
import json
from pathlib import Path
import sys

import duckdb
import pandas as pd


def clean(value):
    if value is pd.NA:
        return {"pandas.NA": True}
    if isinstance(value, (bytes, bytearray)):
        return {type(value).__name__: value.hex()}
    if isinstance(value, (list, tuple)):
        return {type(value).__name__: [clean(item) for item in value]}
    if isinstance(value, dict):
        return {key: clean(item) for key, item in value.items()}
    return value


outcomes = {}


def capture(name, action):
    try:
        outcomes[name] = {"value": clean(action())}
    except Exception as error:
        outcomes[name] = {"error": type(error).__name__, "message": str(error)}


con = duckdb.connect()
buffers = {
    "readonly": memoryview(b"\x00\xffabc"),
    "writable": memoryview(bytearray(b"abc\x00\xff")),
    "empty": memoryview(b""),
    "offset": memoryview(b"0123456789")[2:7],
    "integers": memoryview(array.array("i", [1, -2, 0])),
    "matrix": memoryview(b"abcdef").cast("B", shape=(2, 3)),
    "stride": memoryview(b"abcdef")[::2],
}
for name, view in buffers.items():
    capture(name, lambda view=view: con.sql("select ?, typeof(?)", params=[view, view]).fetchall())


def lifetime():
    source = bytearray(b"before")
    view = memoryview(source)
    con.execute("create table buffers(value blob)")
    con.execute("insert into buffers values (?)", [view])
    source[:] = b"after!"
    view.release()
    del source, view
    gc.collect()
    return con.sql("select * from buffers").fetchall()


capture("owned_input", lifetime)
geometries = [
    "POINT(1 2)", "POINT EMPTY", "POINT Z(1 2 3)", "POINT M(1 2 3)",
    "POINT ZM(1 2 3 4)", "LINESTRING(0 0,1 2)",
    "POLYGON((0 0,1 0,1 1,0 0))", "MULTIPOINT((1 2),(3 4))",
    "GEOMETRYCOLLECTION(POINT(1 2),LINESTRING(0 0,1 1))",
]
for geometry in geometries:
    for method in ["fetchall", "fetchnumpy", "df"]:
        def run(geometry=geometry, method=method):
            relation = con.sql("select ?::geometry g union all select NULL::geometry", params=[geometry])
            result = getattr(relation, method)()
            return result if method == "fetchall" else result["g"].tolist()
        capture(method + "_" + geometry, run)
capture("nested_geometry", lambda: con.sql("select ['POINT(1 2)'::geometry,NULL], {'g':'POINT(3 4)'::geometry}").fetchall())
Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes}, indent=2) + "\n")
print(json.dumps({"cases": len(outcomes)}))
