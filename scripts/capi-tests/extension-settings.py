"""Public two-connection journeys for Parquet setting values, scopes, reset and errors."""

import argparse
import ctypes as C
import hashlib
import json
from pathlib import Path
import subprocess

from arrow_snapshot import Result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--parquet", type=Path, help="reuse an existing identical input for raw error comparisons")
    args = parser.parse_args()
    if args.parquet and not args.parquet.is_file():
        parser.error("--parquet must name an existing shared fixture")
    library, out = args.library.resolve(), args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    subprocess.run(["codesign", "--verify", "--strict", str(library)], check=True)
    original = hashlib.sha256(library.read_bytes()).hexdigest()
    lib, ptr = C.CDLL(str(library)), C.c_void_p

    def fn(name, result, *arguments):
        function = getattr(lib, "duckdb_" + name)
        function.restype, function.argtypes = result, list(arguments)
        return function

    class Info(C.Structure):
        _fields_ = [("image", C.c_char_p), ("base", ptr), ("symbol", C.c_char_p), ("address", ptr)]
    system = C.CDLL("/usr/lib/libSystem.B.dylib")
    system.dladdr.argtypes, system.dladdr.restype = [ptr, C.POINTER(Info)], C.c_int
    info = Info()
    assert system.dladdr(C.cast(lib.duckdb_open, ptr), C.byref(info))
    assert Path(info.image.decode()).resolve() == library

    def query(connection, sql):
        result = Result()
        try:
            status = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))(
                connection, sql.encode(), C.byref(result))
            error = fn("result_error", C.c_char_p, C.POINTER(Result))(C.byref(result))
            record = {"status": status, "error": error.decode() if error else None,
                      "error_type": fn("result_error_type", C.c_uint32, C.POINTER(Result))(C.byref(result)),
                      "rows": []}
            if status == 0:
                columns = fn("column_count", C.c_uint64, C.POINTER(Result))(C.byref(result))
                for row in range(fn("row_count", C.c_uint64, C.POINTER(Result))(C.byref(result))):
                    cells = []
                    for column in range(columns):
                        value = fn("value_varchar", ptr, C.POINTER(Result), C.c_uint64, C.c_uint64)(C.byref(result), column, row)
                        cells.append(C.string_at(value).decode() if value else None)
                        if value:
                            fn("free", None, ptr)(value)
                    record["rows"].append(cells)
            return record
        finally:
            fn("destroy_result", None, C.POINTER(Result))(C.byref(result))

    records = []
    for name, default in [("binary_as_string", False), ("disable_parquet_prefetching", False),
                          ("prefetch_all_parquet_files", False), ("parquet_metadata_cache", False),
                          ("enable_geoparquet_conversion", True)]:
        db, first, second = ptr(), ptr(), ptr()
        assert fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))(None, C.byref(db)) == 0
        for connection in [first, second]:
            assert fn("connect", C.c_int, ptr, C.POINTER(ptr))(db, C.byref(connection)) == 0
        baseline, changed = str(default).lower(), str(not default).lower()
        steps = []
        parquet = args.parquet.resolve() if args.parquet else out / "binary.duckdb.parquet"

        def snapshot(label):
            sql = f"SELECT name,value,scope FROM duckdb_settings() WHERE name='{name}'"
            values = [query(connection, sql) for connection in [first, second]]
            assert all(value["status"] == 0 and len(value["rows"]) == 1 for value in values), values
            record = {"stage": label, "connections": values}
            literal = "'" + str(parquet).replace("'", "''") + "'"
            record["parquet_read"] = [query(connection,
                f"SELECT typeof(b) AS type,b::VARCHAR AS value FROM read_parquet({literal})")
                for connection in [first, second]]
            steps.append(record)

        try:
            if name == "binary_as_string" and not parquet.exists():
                literal = "'" + str(parquet).replace("'", "''") + "'"
                assert query(first, f"COPY (SELECT 'AB'::BLOB AS b) TO {literal} (FORMAT PARQUET)")["status"] == 0
            snapshot("initial")
            for label, sql, connection in [
                ("automatic", f"SET {name}={changed}", first),
                ("reset-automatic", f"RESET {name}", first),
                ("global", f"SET GLOBAL {name}={changed}", first),
                ("session", f"SET SESSION {name}={baseline}", second),
                ("reset-global", f"RESET GLOBAL {name}", first),
                ("local-unimplemented", f"SET LOCAL {name}={changed}", second),
                ("invalid", f"SET {name}='not-bool'", second),
                ("null", f"SET SESSION {name}=NULL", second),
                ("reset-local", f"RESET {name}", second),
            ]:
                result = query(connection, sql)
                steps.append({"stage": label + ":statement", "result": result})
                snapshot(label)
            records.append({"setting": name, "steps": steps})
        finally:
            for connection in [first, second]:
                fn("disconnect", None, C.POINTER(ptr))(C.byref(connection))
            fn("close", None, C.POINTER(ptr))(C.byref(db))
    assert hashlib.sha256(library.read_bytes()).hexdigest() == original
    (out / "results.json").write_text(json.dumps({"library": str(library), "sha256": original,
        "function_image": info.image.decode(), "records": records}, indent=2) + "\n")
    print(json.dumps({"settings": len(records), "out": str(out)}))


if __name__ == "__main__":
    main()
