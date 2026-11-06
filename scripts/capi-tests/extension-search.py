"""Compare ordered extension lookup through public C API calls.

The caller authenticates the library and two C-only extension fixtures first.
Fixtures are raw-v1/c_only_probe.duckdb_extension (version 0.0.1) and raw-v2/
c_only_probe.duckdb_extension (version 0.0.2), both exporting c_only_add7.
"""

import argparse
import ctypes as C
import hashlib
import json
from pathlib import Path

from arrow_snapshot import Result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    parser.add_argument("fixtures", type=Path)
    parser.add_argument("platform")
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    library, fixtures, out = args.library.resolve(), args.fixtures.resolve(), args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    lib = C.CDLL(str(library))
    pointer = C.c_void_p
    slot = C.POINTER(pointer)

    def fn(name, result, *arguments):
        function = getattr(lib, "duckdb_" + name)
        function.restype, function.argtypes = result, list(arguments)
        return function

    free = fn("free", None, pointer)
    destroy = fn("destroy_result", None, C.POINTER(Result))
    quote = lambda path: str(path).replace("'", "''")
    cases = []
    kinds = ["list-second", "custom-then-list", "custom-priority", "list-priority",
             "missing-first-path", "corrupt-first-stops", "install-first",
             "empty-first", "empty-missing", "directory-first"]
    for kind in kinds:
        case = out / kind
        case.mkdir()
        custom, first, second = (case / name for name in ["custom", "first", "second"])
        database, connection, config = pointer(), pointer(), pointer()
        assert fn("create_config", C.c_int, slot)(C.byref(config)) == 0
        for key, value in [(b"allow_unsigned_extensions", b"true"),
                           (b"extension_directory", str(custom).encode())]:
            assert fn("set_config", C.c_int, pointer, C.c_char_p, C.c_char_p)(config, key, value) == 0
        assert fn("open_ext", C.c_int, C.c_char_p, slot, pointer, slot)(
            None, C.byref(database), config, None) == 0
        fn("destroy_config", None, slot)(C.byref(config))
        assert fn("connect", C.c_int, pointer, slot)(database, C.byref(connection)) == 0
        records = []

        def query(label, sql, success=True):
            result = Result()
            try:
                status = fn("query", C.c_int, pointer, C.c_char_p, C.POINTER(Result))(
                    connection, sql.encode(), C.byref(result))
                error = fn("result_error", C.c_char_p, C.POINTER(Result))(C.byref(result))
                rows = []
                if status == 0:
                    for row in range(fn("row_count", C.c_uint64, C.POINTER(Result))(C.byref(result))):
                        values = []
                        for column in range(fn("column_count", C.c_uint64, C.POINTER(Result))(C.byref(result))):
                            value = fn("value_varchar", pointer, C.POINTER(Result), C.c_uint64, C.c_uint64)(
                                C.byref(result), column, row)
                            values.append(C.string_at(value).decode() if value else None)
                            free(value)
                        rows.append(values)
                record = dict(label=kind + "-" + label, sql=sql, status=status,
                              error=error.decode() if error else None, rows=rows)
                records.append(record)
                (out / "progress.json").write_text(json.dumps(cases + [dict(kind=kind, records=records)], indent=2) + "\n")
                assert (status == 0) == success, record
                return rows
            finally:
                destroy(C.byref(result))

        def destination(directory):
            return directory / "v1.5.6" / args.platform / "c_only_probe.duckdb_extension"

        def stage(directory, version="v1", corrupt=False):
            path = destination(directory)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"bad" if corrupt else (fixtures / ("raw-" + version) / path.name).read_bytes())

        try:
            if kind not in ["custom-then-list", "custom-priority"]:
                query("clear-custom", "SET extension_directory=''")
            prefix = "" if kind.startswith("empty-") else quote(first)
            query("set-list", f"SET extension_directories=['{prefix}','{quote(second)}']")
            if kind in ["list-second", "custom-then-list", "empty-first"]:
                stage(second)
            elif kind == "custom-priority":
                stage(custom, "v2")
                stage(first)
                stage(second)
            elif kind == "list-priority":
                stage(first, "v2")
                stage(second)
            elif kind == "corrupt-first-stops":
                stage(first, corrupt=True)
                stage(second)
            elif kind == "directory-first":
                destination(first).mkdir(parents=True)
                stage(second)
            elif kind == "install-first":
                stage(second)
                query("install", f"INSTALL '{quote(fixtures / 'raw-v2/c_only_probe.duckdb_extension')}'")
                assert destination(first).is_file()
            catalog = ("SELECT extension_name,loaded,installed,install_path,extension_version "
                       "FROM duckdb_extensions() WHERE extension_name='c_only_probe'")
            query("catalog-before", catalog)
            success = kind not in ["missing-first-path", "corrupt-first-stops", "empty-missing"]
            query("load", "LOAD c_only_probe", success)
            if success:
                assert query("function", "SELECT c_only_add7(5)") == [["12"]]
                expected = "0.0.2" if kind in ["custom-priority", "list-priority", "install-first"] else "0.0.1"
                assert query("version", "SELECT extension_version FROM duckdb_extensions() WHERE extension_name='c_only_probe'") == [[expected]]
            query("catalog-after", catalog)
            assert query("database-valid", "SELECT 42") == [["42"]]
        finally:
            fn("disconnect", None, slot)(C.byref(connection))
            fn("close", None, slot)(C.byref(database))
        cases.append(dict(kind=kind, records=records))
    (out / "verification.json").write_text(json.dumps(dict(
        cases=cases, library_sha256=hashlib.sha256(library.read_bytes()).hexdigest()), indent=2) + "\n")
    print("TEN_SEARCH_AND_CATALOG_CASES_PASS")


if __name__ == "__main__":
    main()
