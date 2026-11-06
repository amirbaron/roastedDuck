"""Observe public catalog IDs across independent instances and object lifecycles."""

import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path

from arrow_snapshot import Result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    library, out = args.library.resolve(), args.out.resolve()
    assert not out.exists()
    out.mkdir(parents=True)
    ptr = C.c_void_p
    lib = C.CDLL(str(library))

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
    assert Path(os.fsdecode(info.image)).resolve() == library

    def open_database():
        database, connection = ptr(), ptr()
        assert fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))(None, C.byref(database)) == 0
        assert fn("connect", C.c_int, ptr, C.POINTER(ptr))(database, C.byref(connection)) == 0
        return database, connection

    def close_database(database, connection):
        fn("disconnect", None, C.POINTER(ptr))(C.byref(connection))
        fn("close", None, C.POINTER(ptr))(C.byref(database))

    def query(connection, sql):
        result = Result()
        try:
            status = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))(
                connection, sql.encode(), C.byref(result))
            error = fn("result_error", C.c_char_p, C.POINTER(Result))(C.byref(result))
            if status:
                return {"state": status, "error": error.decode() if error else None}
            rows = []
            for row in range(fn("row_count", C.c_uint64, C.POINTER(Result))(C.byref(result))):
                cells = []
                for column in range(fn("column_count", C.c_uint64, C.POINTER(Result))(C.byref(result))):
                    value = fn("value_varchar", ptr, C.POINTER(Result), C.c_uint64, C.c_uint64)(
                        C.byref(result), column, row)
                    cells.append(C.string_at(value).decode() if value else None)
                    if value:
                        fn("free", None, ptr)(value)
                rows.append(cells)
            return {"state": status, "rows": rows}
        finally:
            fn("destroy_result", None, C.POINTER(Result))(C.byref(result))

    connections = [open_database(), open_database()]
    observations = []
    snapshots = [
        "PRAGMA database_list",
        "SELECT database_name,database_oid FROM duckdb_databases() ORDER BY database_name",
        "SELECT schema_name,oid FROM duckdb_schemas() WHERE database_name='memory' ORDER BY schema_name",
        "SELECT table_name,table_oid,schema_oid FROM duckdb_tables() WHERE database_name='memory' ORDER BY table_name",
    ]

    def snapshot(label, connection):
        observations.append({"stage": label, "queries": [
            {"sql": sql, **query(connection, sql)} for sql in snapshots]})

    for index, (_, connection) in enumerate(connections):
        snapshot("initial-instance-" + str(index), connection)
    connection = connections[0][1]
    for stage, sql in [
        ("create", "CREATE SCHEMA s; CREATE TABLE s.t(i INTEGER); CREATE TABLE main.u(i INTEGER)"),
        ("failed-create", "CREATE TABLE s.t(i INTEGER)"),
        ("if-not-exists", "CREATE TABLE IF NOT EXISTS s.t(i INTEGER)"),
        ("rename", "ALTER TABLE s.t RENAME TO renamed"),
        ("comment", "COMMENT ON TABLE s.renamed IS 'metadata'"),
        ("drop-create", "DROP TABLE main.u; CREATE TABLE main.v(i INTEGER)"),
    ]:
        observations.append({"stage": stage + "-operation", "sql": sql, **query(connection, sql)})
        snapshot(stage, connection)
    snapshot("untouched-instance-1", connections[1][1])
    for database, connection in connections:
        close_database(database, connection)
    database, connection = open_database()
    snapshot("new-instance-after-closing", connection)
    close_database(database, connection)
    report = {"library": str(library), "function_image": os.fsdecode(info.image),
              "library_sha256": hashlib.sha256(library.read_bytes()).hexdigest(),
              "version": fn("library_version", C.c_char_p)().decode(), "observations": observations}
    (out / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"out": str(out), "stages": len(observations)}))


if __name__ == "__main__":
    main()
