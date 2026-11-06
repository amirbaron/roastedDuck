"""Trace actual public pending tasks and progress, with materialized/streaming results."""

import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import sys

from arrow_snapshot import Result


class Progress(C.Structure):
    _fields_ = [("percentage", C.c_double), ("rows_processed", C.c_uint64),
                ("total_rows_to_process", C.c_uint64)]


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("library", type=Path)
parser.add_argument("output", type=Path)
parser.add_argument("--exhaust", action="store_true", help="also fetch EOF before closing the result")
args = parser.parse_args()
if args.output.exists():
    parser.error("preserve existing evidence; use a new output path")
library = args.library.resolve()
lib = C.CDLL(str(library))
ptr, ref = C.c_void_p, C.byref
function_image = None
if sys.platform == "darwin":
    class DlInfo(C.Structure):
        _fields_ = [("image", C.c_char_p), ("base", ptr), ("symbol", C.c_char_p), ("address", ptr)]

    system = C.CDLL("/usr/lib/libSystem.B.dylib")
    system.dladdr.argtypes, system.dladdr.restype = [ptr, C.POINTER(DlInfo)], C.c_int
    info = DlInfo()
    assert system.dladdr(C.cast(lib.duckdb_query_progress, ptr), ref(info)) != 0
    function_image = Path(os.fsdecode(info.image)).resolve()
    assert function_image == library, (function_image, library)


def fn(name, result, *arguments):
    value = getattr(lib, "duckdb_" + name)
    value.restype, value.argtypes = result, list(arguments)
    return value


db, con = ptr(), ptr()
assert fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))(None, ref(db)) == 0
assert fn("connect", C.c_int, ptr, C.POINTER(ptr))(db, ref(con)) == 0
query = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))
destroy = fn("destroy_result", None, C.POINTER(Result))


def run(sql):
    result = Result()
    try:
        assert query(con, sql, ref(result)) == 0
    finally:
        destroy(ref(result))


# One thread is local to this task-stepping diagnostic, never a corpus-wide override.
run(b"SET threads=1; SET enable_progress_bar=true; SET enable_progress_bar_print=false;")
progress = fn("query_progress", Progress, ptr)


def snapshot(stage, task_state=None):
    value = progress(con)
    return {"stage": stage, "task_state": task_state,
            **{name: getattr(value, name) for name, _ in Progress._fields_}}


rows = []
for mode in ["materialized", "streaming"]:
    for label, sql, expected in [
        ("constant", b"SELECT 42::BIGINT AS answer", 42),
        ("empty-count", b"SELECT count(*)::BIGINT FROM range(0)", 0),
        ("range-sum", b"SELECT sum(i % 97)::BIGINT FROM range(1000000)t(i)", 47999055),
    ]:
        prepared, pending = ptr(), ptr()
        result = Result()
        steps = [snapshot("idle")]
        try:
            assert fn("prepare", C.c_int, ptr, C.c_char_p, C.POINTER(ptr))(con, sql, ref(prepared)) == 0
            steps.append(snapshot("prepared"))
            prepare_pending = "pending_prepared_streaming" if mode == "streaming" else "pending_prepared"
            assert fn(prepare_pending, C.c_int, ptr, C.POINTER(ptr))(prepared, ref(pending)) == 0
            steps.append(snapshot("pending"))
            for _ in range(10000):
                state = fn("pending_execute_task", C.c_int, ptr)(pending)
                steps.append(snapshot("task", state))
                if state == 0:
                    break
                assert state in [1, 3], state
            else:
                raise RuntimeError("bounded pending task diagnostic exceeded 10000steps")
            assert fn("execute_pending", C.c_int, ptr, C.POINTER(Result))(pending, ref(result)) == 0
            is_streaming = fn("result_is_streaming", C.c_bool, Result)(result)
            steps.append(snapshot("executed"))
            chunk = ptr(fn("fetch_chunk", ptr, Result)(result))
            try:
                assert fn("data_chunk_get_size", C.c_uint64, ptr)(chunk) == 1
                vector = fn("data_chunk_get_vector", ptr, ptr, C.c_uint64)(chunk, 0)
                data = fn("vector_get_data", ptr, ptr)(vector)
                scalar = C.cast(data, C.POINTER(C.c_int64))[0]
                assert scalar == expected, (label, scalar, expected)
            finally:
                fn("destroy_data_chunk", None, C.POINTER(ptr))(ref(chunk))
            steps.append(snapshot("fetched"))
            if args.exhaust:
                assert not fn("fetch_chunk", ptr, Result)(result)
                steps.append(snapshot("exhausted"))
        finally:
            destroy(ref(result))
            fn("destroy_pending", None, C.POINTER(ptr))(ref(pending))
            fn("destroy_prepare", None, C.POINTER(ptr))(ref(prepared))
        steps.append(snapshot("closed"))
        rows.append({"case": f"{mode}/{label}", "scalar": scalar,
                     "result_is_streaming": is_streaming, "steps": steps})
run(b"SELECT 42")
fn("disconnect", None, C.POINTER(ptr))(ref(con))
fn("close", None, C.POINTER(ptr))(ref(db))
args.output.write_text(json.dumps({"library": str(library),
                                  "query_progress_function_image": str(function_image) if function_image else None,
                                  "sha256": hashlib.sha256(library.read_bytes()).hexdigest(),
                                  "threads": 1, "fetch_eof_before_close": args.exhaust,
                                  "cases": rows, "reuse": True}, indent=2) + "\n")
print(json.dumps({"cases": len(rows), "all_scalars_and_reuse": True}))
