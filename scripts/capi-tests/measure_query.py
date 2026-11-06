"""Bounded C ABI query timing and peak RSS, run in a fresh process per library."""
import argparse
import ctypes as C
import json
import resource
import sys
import time

from arrow_snapshot import Result

parser = argparse.ArgumentParser()
parser.add_argument("library")
parser.add_argument("--iterations", type=int, default=1000)
args = parser.parse_args()
assert args.iterations > 0
lib = C.CDLL(args.library)
ptr, ref = C.c_void_p, C.byref

def fn(name, result, *arguments):
    value = getattr(lib, "duckdb_" + name)
    value.restype, value.argtypes = result, list(arguments)
    return value

db, con = ptr(), ptr()
assert fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))(None, ref(db)) == 0
assert fn("connect", C.c_int, ptr, C.POINTER(ptr))(db, ref(con)) == 0
query = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))
destroy = fn("destroy_result", None, C.POINTER(Result))
get = fn("value_int64", C.c_int64, C.POINTER(Result), C.c_uint64, C.c_uint64)

def run(sql):
    result = Result()
    assert query(con, sql, ref(result)) == 0
    value = get(ref(result), 0, 0)
    destroy(ref(result))
    return value

run(b"SET threads=1")
assert run(b"SELECT 42") == 42
start = time.perf_counter()
checksum = sum(run(b"SELECT 42") for _ in range(args.iterations))
elapsed = time.perf_counter() - start
assert checksum == args.iterations * 42
fn("disconnect", None, C.POINTER(ptr))(ref(con))
fn("close", None, C.POINTER(ptr))(ref(db))
peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
if sys.platform != "darwin":
    peak *= 1024
print(json.dumps(dict(library=args.library, iterations=args.iterations,
                     elapsed_seconds=elapsed, peak_rss_bytes=peak, checksum=checksum)))
