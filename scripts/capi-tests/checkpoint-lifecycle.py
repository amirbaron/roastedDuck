"""Observe real WAL effects of direct/prepared/pending checkpoints and errors."""

import argparse
from collections import Counter
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import shutil
import time

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
    assert system.dladdr(C.cast(lib.duckdb_execute_prepared, ptr), C.byref(info))
    assert Path(os.fsdecode(info.image)).resolve() == library
    records = []
    for mode in ["direct", "prepared", "pending", "pending-streaming"]:
        directory = out / mode
        directory.mkdir()
        file = directory / "checkpoint.duckdb"
        wal = Path(str(file) + ".wal")
        db, connection, prepared = ptr(), ptr(), ptr()
        assert fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))(os.fsencode(file), C.byref(db)) == 0
        assert fn("connect", C.c_int, ptr, C.POINTER(ptr))(db, C.byref(connection)) == 0

        def describe(state, result):
            error = fn("result_error", C.c_char_p, C.POINTER(Result))(C.byref(result))
            return {"state": state, "error_type": fn("result_error_type", C.c_uint32, C.POINTER(Result))(C.byref(result)),
                    "error": error.decode() if error else None,
                    "streaming": fn("result_is_streaming", C.c_bool, Result)(result)}

        def query(sql):
            result = Result()
            try:
                state = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))(
                    connection, sql.encode(), C.byref(result))
                record = describe(state, result)
                assert not state, record
                return record
            finally:
                fn("destroy_result", None, C.POINTER(Result))(C.byref(result))

        query("PRAGMA disable_checkpoint_on_shutdown; SET checkpoint_threshold='1GB'; CREATE TABLE t(i INTEGER)")
        if mode != "direct":
            assert fn("prepare", C.c_int, ptr, C.c_char_p, C.POINTER(ptr))(
                connection, b"CHECKPOINT", C.byref(prepared)) == 0

        def checkpoint():
            if mode == "direct":
                return query("CHECKPOINT")
            result, pending = Result(), ptr()
            try:
                if mode == "prepared":
                    state = fn("execute_prepared", C.c_int, ptr, C.POINTER(Result))(prepared, C.byref(result))
                else:
                    make = "pending_prepared_streaming" if mode == "pending-streaming" else "pending_prepared"
                    assert fn(make, C.c_int, ptr, C.POINTER(ptr))(prepared, C.byref(pending)) == 0
                    deadline = time.monotonic() + 30
                    states = Counter()
                    while time.monotonic() < deadline:
                        task = fn("pending_execute_task", C.c_int, ptr)(pending)
                        states[task] += 1
                        if task in [0, 2]:  # RESULT_READY / ERROR
                            break
                        if task == 3:  # NO_TASKS_AVAILABLE: wait for work/IO readiness.
                            time.sleep(0.001)
                    else:
                        raise AssertionError("checkpoint did not finish bounded task stepping")
                    state = fn("execute_pending", C.c_int, ptr, C.POINTER(Result))(pending, C.byref(result))
                return describe(state, result)
            finally:
                fn("destroy_result", None, C.POINTER(Result))(C.byref(result))
                if pending:
                    fn("destroy_pending", None, C.POINTER(ptr))(C.byref(pending))

        steps = []
        for iteration in range(2):
            query(f"INSERT INTO t VALUES ({iteration})")
            assert wal.stat().st_size > 0
            snapshot = directory / ("before-" + str(iteration))
            snapshot.mkdir()
            for path in [file, wal]:
                shutil.copy2(path, snapshot / path.name)
            result = checkpoint()
            after = wal.stat().st_size if wal.exists() else 0
            steps.append({"iteration": iteration, "result": result, "wal_empty_after": after == 0})
        query("BEGIN; INSERT INTO t VALUES (99)")
        if mode == "direct":
            result = Result()
            state = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))(
                connection, b"CHECKPOINT", C.byref(result))
            failed = describe(state, result)
            fn("destroy_result", None, C.POINTER(Result))(C.byref(result))
        else:
            failed = checkpoint()
        query("ROLLBACK")
        recovered = checkpoint()
        fn("destroy_prepare", None, C.POINTER(ptr))(C.byref(prepared))
        fn("disconnect", None, C.POINTER(ptr))(C.byref(connection))
        fn("close", None, C.POINTER(ptr))(C.byref(db))
        records.append({"mode": mode, "steps": steps, "uncommitted_error": failed, "after_rollback": recovered})
        (out / "completed-cases.json").write_text(json.dumps(records, indent=2) + "\n")
    report = {"library": str(library), "function_image": os.fsdecode(info.image),
              "library_sha256": hashlib.sha256(library.read_bytes()).hexdigest(),
              "version": fn("library_version", C.c_char_p)().decode(), "cases": records}
    (out / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"cases": len(records), "out": str(out)}))


if __name__ == "__main__":
    main()
