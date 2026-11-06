"""Real NumPy parameter shapes, Arrow protocol rejection and typed JSON errors."""
import json
from pathlib import Path
import sys

import duckdb
import numpy as np
import pyarrow as pa
import pyarrow.ipc as ipc

outcomes = {}


def clean(value):
    if isinstance(value, (list, tuple)):
        return {type(value).__name__: [clean(item) for item in value]}
    if isinstance(value, dict):
        return {str(key): clean(item) for key, item in value.items()}
    if isinstance(value, bytes):
        return {"bytes": value.hex()}
    return value


def capture(name, action):
    try:
        outcomes[name] = {"value": clean(action())}
    except Exception as error:
        outcomes[name] = {"error": type(error).__name__, "message": str(error)}


for name, value in [
    ("empty", np.array([], dtype="int64")),
    ("scalar", np.array(42, dtype="int32")),
    ("ints", np.arange(6, dtype="int16")),
    ("matrix", np.arange(6, dtype="int32").reshape(2, 3)),
    ("cube", np.arange(8, dtype="uint8").reshape(2, 2, 2)),
    ("float", np.array([1.5, 2.25], dtype="float32")),
    ("strings", np.array(["雪", "a\0b"])),
    ("strided", np.arange(10, dtype="int64")[::2]),
    ("reversed", np.arange(10, dtype="int64")[::-1]),
]:
    def parameter(value=value):
        con = duckdb.connect()
        return con.execute("select ?, typeof(?)", [value, value]).fetchall()
    capture("parameter_" + name, parameter)


def schema_capsule():
    con = duckdb.connect()
    capsule = pa.schema([("x", pa.int64())]).__arrow_c_schema__()
    return con.sql("select * from capsule").fetchall()


capture("wrong_capsule", schema_capsule)


def message_reader():
    con = duckdb.connect()
    batch = pa.record_batch([pa.array([1, 2, None])], names=["x"])
    sink = pa.BufferOutputStream()
    with ipc.new_stream(sink, batch.schema) as writer:
        writer.write_batch(batch)
    reader = ipc.MessageReader.open_stream(pa.BufferReader(sink.getvalue()))
    return con.from_arrow(reader).fetchall()


capture("ipc_message_reader", message_reader)

for method in ["execute", "sql", "extract_statements", "get_table_names"]:
    for sql in ["syntax error", "select * from absent_tail_table", "select absent_column", "select 'not_int'::integer", "select error('tail error')"]:
        def error_query(method=method, sql=sql):
            con = duckdb.connect(config={"errors_as_json": True})
            result = getattr(con, method)(sql)
            return result.fetchall() if hasattr(result, "fetchall") else type(result).__name__
        capture(method + "_" + sql, error_query)

Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes}, indent=2, ensure_ascii=False) + "\n")
print(json.dumps({"cases": len(outcomes)}))
