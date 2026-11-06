"""Observe exact public collation results, metadata and errors on one native engine."""

import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import subprocess

from arrow_snapshot import Result


def cases():
    values = "(VALUES (1,'A'),(2,'a'),(3,'É'),(4,'E'),(5,'é'),(6,'é'),(7,'Straße'),(8,'STRASSE'),(9,NULL)) AS t(i,v)"
    yield "inventory", "SELECT collname FROM pragma_collations() ORDER BY collname"
    for collation in ["nocase", "noaccent", "nfc", "nocase.noaccent", "noaccent.nocase",
                      "nocase.nfc", "nfc.nocase", "en", "de", "tr", "ja", "zh"]:
        yield collation + ":order", f"SELECT i,v FROM {values} ORDER BY v COLLATE {collation} NULLS LAST,i"
        yield collation + ":distinct", f"SELECT count(DISTINCT v COLLATE {collation}) AS n FROM {values}"
        yield collation + ":groups", f"SELECT count(*) AS n FROM {values} GROUP BY v COLLATE {collation} ORDER BY n"
        yield collation + ":aggregate-order", f"SELECT list(i ORDER BY v COLLATE {collation} NULLS LAST,i) AS ids FROM {values}"
        yield collation + ":window", f"SELECT i,count(*) OVER(PARTITION BY v COLLATE {collation}) AS n FROM {values} ORDER BY i"
        yield collation + ":like", f"SELECT i FROM {values} WHERE v COLLATE {collation} LIKE 'e%' ORDER BY i"
    for collation in ["NOCASE", "nocase.nocase", "nfc.noaccent", "nfc.de", "de.nfc", "missing_collation", "nocase.missing_collation"]:
        yield "resolve:" + collation, f"SELECT 'É' COLLATE {collation} = 'e' AS equal"
    yield "default", "SET default_collation='noaccent.nocase'; SELECT 'É'='e' AS equal"
    yield "reset", "RESET default_collation; SELECT 'É'='e' AS equal"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    library, out = args.library.resolve(), args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    subprocess.run(["codesign", "--verify", "--strict", str(library)], check=True)
    original = hashlib.sha256(library.read_bytes()).hexdigest()
    lib, ptr = C.CDLL(str(library)), C.c_void_p

    def fn(name, result, *arguments):
        value = getattr(lib, "duckdb_" + name)
        value.restype, value.argtypes = result, list(arguments)
        return value

    class Info(C.Structure):
        _fields_ = [("image", C.c_char_p), ("base", ptr), ("symbol", C.c_char_p), ("address", ptr)]

    system = C.CDLL("/usr/lib/libSystem.B.dylib")
    system.dladdr.argtypes, system.dladdr.restype = [ptr, C.POINTER(Info)], C.c_int
    info = Info()
    assert system.dladdr(C.cast(lib.duckdb_open, ptr), C.byref(info))
    assert Path(os.fsdecode(info.image)).resolve() == library
    db, con = ptr(), ptr()
    assert fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))(None, C.byref(db)) == 0
    assert fn("connect", C.c_int, ptr, C.POINTER(ptr))(db, C.byref(con)) == 0
    records = []
    try:
        for name, sql in cases():
            result = Result()
            try:
                state = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))(
                    con, sql.encode(), C.byref(result))
                error = fn("result_error", C.c_char_p, C.POINTER(Result))(C.byref(result))
                record = {"case": name, "sql": sql, "state": state,
                          "error": error.decode() if error else None,
                          "error_type": fn("result_error_type", C.c_uint32, C.POINTER(Result))(C.byref(result)),
                          "columns": [], "rows": []}
                if state == 0:
                    columns = fn("column_count", C.c_uint64, C.POINTER(Result))(C.byref(result))
                    for index in range(columns):
                        column = fn("column_name", C.c_char_p, C.POINTER(Result), C.c_uint64)(C.byref(result), index)
                        kind = fn("column_type", C.c_uint32, C.POINTER(Result), C.c_uint64)(C.byref(result), index)
                        record["columns"].append([column.decode(), kind])
                    for row in range(fn("row_count", C.c_uint64, C.POINTER(Result))(C.byref(result))):
                        cells = []
                        for column in range(columns):
                            value = fn("value_varchar", ptr, C.POINTER(Result), C.c_uint64, C.c_uint64)(C.byref(result), column, row)
                            cells.append(C.string_at(value).decode() if value else None)
                            if value:
                                fn("free", None, ptr)(value)
                        record["rows"].append(cells)
                records.append(record)
            finally:
                fn("destroy_result", None, C.POINTER(Result))(C.byref(result))
    finally:
        fn("disconnect", None, C.POINTER(ptr))(C.byref(con))
        fn("close", None, C.POINTER(ptr))(C.byref(db))
    assert hashlib.sha256(library.read_bytes()).hexdigest() == original
    (out / "results.json").write_text(json.dumps({"library": str(library), "sha256": original,
        "function_image": os.fsdecode(info.image), "records": records}, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"cases": len(records), "out": str(out)}))


if __name__ == "__main__":
    main()
