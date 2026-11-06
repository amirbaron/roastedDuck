"""Observe Pandas 3 strings through public clients; compare both engines' records.

Each successful scan checks independent values, mutation and retained ownership.
An exception is recorded so a before-fix run preserves every failing case. A zero
process exit means collection succeeded; acceptance requires matching C++ records.
"""
import gc
import ctypes
import importlib
import json
from pathlib import Path
import sys

from native_hashing import sha256_stream

engine, native_path, expected, destination = sys.argv[1:]
native_path = Path(native_path).resolve()
with native_path.open("rb") as stream:
    assert sha256_stream(stream) == expected

import duckdb
import pandas as pd

assert int(pd.__version__.split(".")[0]) >= 3
native = importlib.import_module("duckdb._duckdb" if engine == "rust" else "_duckdb")
assert Path(native.__file__).resolve() == native_path
assert bool(getattr(native, "__rust_engine__", False)) == (engine == "rust")

try:
    import pyarrow as pa
except ModuleNotFoundError:
    pa = None

descriptors = [
    ("default", None),
    ("str", "str"),
    ("python-nan", pd.StringDtype(storage="python", na_value=float("nan"))),
    ("python-NA", pd.StringDtype(storage="python", na_value=pd.NA)),
]
if pa is not None:
    descriptors.extend([
        ("pyarrow-nan", pd.StringDtype(storage="pyarrow", na_value=float("nan"))),
        ("pyarrow-NA", pd.StringDtype(storage="pyarrow", na_value=pd.NA)),
    ])

vectors = [
    ("unicode-nul-null", ["duck", "雪", None, "a\x00b", "", "🌍"]),
    ("empty", []),
    ("all-null", [None, None]),
    ("vector-boundaries", (["duck", "雪", None, "a\x00b"] * 1537)[13:4110]),
]
records = []
for descriptor, dtype in descriptors:
    for case, values in vectors:
        # Empty/all-NULL values cannot infer a string type. Their explicit
        # StringDtype cases below test that contract without assuming inference.
        if dtype is None and case in {"empty", "all-null"}:
            continue
        frame = pd.DataFrame({"x": pd.Series(values, dtype=dtype)})
        record = {
            "descriptor": descriptor,
            "case": case,
            "dtype": str(frame.x.dtype),
            "storage": getattr(frame.x.dtype, "storage", None),
        }
        with duckdb.connect(config={"threads": 1}) as connection:
            try:
                relation = connection.from_df(frame)
                expected_rows = [(value,) for value in values]
                assert relation.columns == ["x"]
                assert [str(value) for value in relation.types] == ["VARCHAR"]
                rows = relation.fetchall()
                assert rows == expected_rows, (rows[:8], expected_rows[:8])
                record["rows"] = rows
                if values:
                    frame.loc[0, "x"] = "NEW"
                    assert frame.loc[0, "x"] == "NEW"
                    # Pandas 3 copy-on-write preserves the relation's bound
                    # frame, as observed on the frozen C++ client.
                    rows = relation.fetchall()
                    assert rows == expected_rows, (rows[:8], expected_rows[:8])
                    record["mutated_rows"] = rows
                del frame
                gc.collect()
                retained = relation.fetchall()
                assert retained == expected_rows
                record["retained_rows"] = retained
                connection.close()
                try:
                    relation.fetchall()
                except Exception as error:
                    record["closed_connection_error"] = {
                        "type": type(error).__name__, "message": str(error)
                    }
                else:
                    raise AssertionError("closed connection remained usable")
            except Exception as error:
                record["error"] = {"type": type(error).__name__, "message": str(error)}
        records.append(record)

if sys.platform == "darwin":
    system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    system._dyld_image_count.restype = ctypes.c_uint32
    system._dyld_get_image_name.argtypes = [ctypes.c_uint32]
    system._dyld_get_image_name.restype = ctypes.c_char_p
    images = set()
    for index in range(system._dyld_image_count()):
        path = Path(system._dyld_get_image_name(index).decode()).resolve()
        if path.name.startswith("_duckdb") or path.name == "libduckdb.dylib":
            images.add(str(path))
    assert images == {str(native_path)}, images
elif sys.platform.startswith("linux"):
    images = set()
    for line in Path("/proc/self/maps").read_text().splitlines():
        fields = line.split(None, 5)
        if len(fields) == 6 and fields[5].startswith("/"):
            path = Path(fields[5].removesuffix(" (deleted)")).resolve()
            if path.name.startswith("_duckdb") or path.name == "libduckdb.so":
                images.add(str(path))
    assert images == {str(native_path)}, images
with native_path.open("rb") as stream:
    assert sha256_stream(stream) == expected
report = {
    "engine": engine, "pandas": pd.__version__,
    "pyarrow": pa.__version__ if pa is not None else None,
    "native_sha256": expected, "records": records,
    "scope": "Public value/mutation/lifetime collection; compare raw C++/Rust records for acceptance",
}
Path(destination).write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
print(json.dumps({"engine": engine, "cases": len(records),
                  "errors": sum("error" in row for row in records)}), flush=True)
