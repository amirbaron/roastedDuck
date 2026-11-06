"""Cross-engine checkpoint/WAL reopening with scalar defaults, generated columns and indexes."""

import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from arrow_snapshot import Result


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def child(library, mode, database, output):
    ptr = C.c_void_p
    lib = C.CDLL(str(library))

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
    db, con, error = ptr(), ptr(), ptr()
    status = fn("open_ext", C.c_int, C.c_char_p, C.POINTER(ptr), ptr, C.POINTER(ptr))(
        os.fsencode(database), C.byref(db), None, C.byref(error))
    message = C.string_at(error).decode() if error else None
    if error:
        fn("free", None, ptr)(error)
    assert status == 0, message
    assert fn("connect", C.c_int, ptr, C.POINTER(ptr))(db, C.byref(con)) == 0

    def query(sql, *, failure=False):
        result = Result()
        try:
            status = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))(
                con, sql.encode(), C.byref(result))
            error = fn("result_error", C.c_char_p, C.POINTER(Result))(C.byref(result))
            error_type = fn("result_error_type", C.c_uint32, C.POINTER(Result))(C.byref(result))
            record = {"status": status, "error": error.decode() if error else None,
                      "error_type": error_type, "rows": []}
            if failure:
                assert status == 1 and error_type == 18, record
            else:
                assert status == 0, record
                rows = fn("row_count", C.c_uint64, C.POINTER(Result))(C.byref(result))
                columns = fn("column_count", C.c_uint64, C.POINTER(Result))(C.byref(result))
                for row in range(rows):
                    cells = []
                    for column in range(columns):
                        value = fn("value_varchar", ptr, C.POINTER(Result), C.c_uint64, C.c_uint64)(
                            C.byref(result), column, row)
                        cells.append(C.string_at(value).decode() if value else None)
                        if value:
                            fn("free", None, ptr)(value)
                    record["rows"].append(cells)
            return record
        finally:
            fn("destroy_result", None, C.POINTER(Result))(C.byref(result))

    report = {"library": str(library), "library_sha256": digest(library),
              "function_image": os.fsdecode(info.image), "mode": mode}
    try:
        query("SET threads=1; SET checkpoint_threshold='1GB'; PRAGMA disable_checkpoint_on_shutdown")
        sql = "SELECT i,j,s FROM t ORDER BY i NULLS LAST"
        expected = [["-7", "7", "abc"], ["3", "3", "abc"], [None, None, "abc"]]
        if mode.startswith("write"):
            query("CREATE TABLE t(i INTEGER, j INTEGER GENERATED ALWAYS AS (abs(i)), "
                  "s VARCHAR DEFAULT lower('ABC'), CHECK (abs(i)<=1000))")
            query("CREATE INDEX scalar_idx ON t((abs(i)))")
            query("CHECKPOINT; INSERT INTO t(i) VALUES (-7),(3),(NULL)")
            report["rows"] = query(sql)["rows"]
            assert report["rows"] == expected
            if mode == "write-checkpoint":
                query("CHECKPOINT")
        else:
            if mode == "readback":
                expected.insert(0, ["-8", "8", "abc"])
            report["rows"] = query(sql)["rows"]
            assert report["rows"] == expected, report
            report["functional_index"] = query("SELECT i,j FROM t WHERE abs(i)=7")["rows"]
            assert report["functional_index"] == [["-7", "7"]]
            report["index"] = query("SELECT index_name FROM duckdb_indexes() WHERE index_name='scalar_idx'")["rows"]
            assert report["index"] == [["scalar_idx"]]
            if mode == "read-write":
                report["constraint_error"] = query("INSERT INTO t(i) VALUES (-1001)", failure=True)
                query("INSERT INTO t(i) VALUES (-8)")
                report["after"] = query(sql)["rows"]
                assert report["after"] == [["-8", "8", "abc"], *expected]
                query("CHECKPOINT")
        output.write_text(json.dumps(report, indent=2) + "\n")
        if mode == "write-wal":
            # Preserve committed WAL without a graceful shutdown/replay in this process.
            os._exit(0)
    finally:
        fn("disconnect", None, C.POINTER(ptr))(C.byref(con))
        fn("close", None, C.POINTER(ptr))(C.byref(db))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cpp", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    libraries = {"cpp": args.cpp.resolve(), "rust": args.rust.resolve()}
    inputs = {}
    for name, library in libraries.items():
        subprocess.run(["codesign", "--verify", "--strict", str(library)], check=True)
        inputs[name] = {"library": str(library), "sha256": digest(library)}
    (out / "manifest.json").write_text(json.dumps(inputs, indent=2) + "\n")
    cases = []
    for writer, reader in [("cpp", "rust"), ("rust", "cpp")]:
        for mode in ["checkpoint", "wal"]:
            directory = out / f"{writer}-{mode}"
            directory.mkdir()
            (directory / "tmp").mkdir()
            database = directory / "scalar.duckdb"

            def run(engine, stage):
                output = directory / (stage + ".json")
                with (directory / (stage + ".log")).open("x") as log:
                    result = subprocess.run(
                        [sys.executable, str(Path(__file__).resolve()), "child", str(libraries[engine]),
                         stage, str(database), str(output)], stdout=log, stderr=subprocess.STDOUT,
                        env=dict(os.environ, TZ="UTC", TMPDIR=str(directory / "tmp")),
                        timeout=60, preexec_fn=lambda: os.nice(10),
                    )
                (directory / (stage + "-terminal.json")).write_text(
                    json.dumps({"exit_code": result.returncode}) + "\n")
                assert result.returncode == 0, directory / (stage + ".log")
                return json.loads(output.read_text())

            written = run(writer, "write-" + mode)
            snapshot = directory / "before-replay"
            snapshot.mkdir()
            hashes = {}
            for file in [database, Path(str(database) + ".wal")]:
                if file.exists():
                    shutil.copy2(file, snapshot / file.name)
                    hashes[file.name] = {"sha256": digest(file), "bytes": file.stat().st_size}
            if mode == "wal":
                assert hashes[database.name + ".wal"]["bytes"] > 0
            received = run(reader, "read-write")
            readback = run(writer, "readback")
            assert written["rows"] == received["rows"]
            assert received["after"] == readback["rows"]
            cases.append({"writer": writer, "reader": reader, "mode": mode,
                          "snapshots": hashes, "directory": str(directory), "passed": True})
    for name, library in libraries.items():
        assert digest(library) == inputs[name]["sha256"]
        subprocess.run(["codesign", "--verify", "--strict", str(library)], check=True)
    (out / "summary.json").write_text(json.dumps({"passed": len(cases), "cases": cases}, indent=2) + "\n")
    print(json.dumps({"passed": len(cases), "output": str(out)}))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "child":
        child(Path(sys.argv[2]), sys.argv[3], Path(sys.argv[4]), Path(sys.argv[5]))
    else:
        main()
