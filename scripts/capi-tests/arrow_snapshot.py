"""Public C ABI journey: result Arrow settings survive subsequent connection SETs.

Run separately against each signed library; compare the JSON outputs. No DuckDB
Python module is imported. Uses only duckdb.h and the Arrow C data interface.
"""
import argparse
import ctypes as C
import json
from pathlib import Path
import sys


class Result(C.Structure):
    _fields_ = [(n, C.c_uint64) for n in ("columns", "rows", "changed")] + [
        (n, C.c_void_p) for n in ("deprecated_columns", "error", "internal")
    ]


class Schema(C.Structure):
    pass


Schema._fields_ = [
    ("format", C.c_char_p), ("name", C.c_char_p), ("metadata", C.c_void_p),
    ("flags", C.c_int64), ("n_children", C.c_int64),
    ("children", C.POINTER(C.POINTER(Schema))), ("dictionary", C.POINTER(Schema)),
    ("release", C.c_void_p), ("private_data", C.c_void_p),
]


class Array(C.Structure):
    pass


Array._fields_ = [(n, C.c_int64) for n in (
    "length", "null_count", "offset", "n_buffers", "n_children"
)] + [
    ("buffers", C.POINTER(C.c_void_p)), ("children", C.POINTER(C.POINTER(Array))),
    ("dictionary", C.POINTER(Array)), ("release", C.c_void_p), ("private_data", C.c_void_p),
]


def schema_json(schema):
    # Arrow metadata is a length-prefixed binary key/value map.
    metadata, address = [], schema.metadata
    if address:
        count = C.c_int32.from_address(address).value
        address += 4
        for _ in range(count):
            pair = []
            for _ in range(2):
                size = C.c_int32.from_address(address).value
                address += 4
                pair.append(C.string_at(address, size).decode())
                address += size
            metadata.append(pair)
    return {"format": schema.format.decode(), "name": (schema.name or b"").decode(),
            "metadata": metadata,
            "children": [schema_json(schema.children[i].contents) for i in range(schema.n_children)],
            "dictionary": schema_json(schema.dictionary.contents) if schema.dictionary else None}


def release(value):
    if value.release:
        C.CFUNCTYPE(None, C.POINTER(type(value)))(value.release)(C.byref(value))
        assert not value.release


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("library")
    parser.add_argument("--reference", type=Path, help="Require exact agreement with a saved C++ run")
    args = parser.parse_args()
    lib = C.CDLL(args.library)
    ptr, u64, ref = C.c_void_p, C.c_uint64, C.byref

    def fn(name, result, *arguments):
        value = getattr(lib, "duckdb_" + name)
        value.restype, value.argtypes = result, list(arguments)
        return value

    open_db = fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))
    connect = fn("connect", C.c_int, ptr, C.POINTER(ptr))
    query = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))
    destroy = fn("destroy_result", None, C.POINTER(Result))
    prepare = fn("prepare", C.c_int, ptr, C.c_char_p, C.POINTER(ptr))
    pending = fn("pending_prepared", C.c_int, ptr, C.POINTER(ptr))
    execute_pending = fn("execute_pending", C.c_int, ptr, C.POINTER(Result))
    execute = fn("execute_prepared", C.c_int, ptr, C.POINTER(Result))
    stream = fn("execute_prepared_streaming", C.c_int, ptr, C.POINTER(Result))
    options = fn("result_get_arrow_options", ptr, C.POINTER(Result))
    current_options = fn("connection_get_arrow_options", None, ptr, C.POINTER(ptr))
    logical = fn("column_logical_type", ptr, C.POINTER(Result), u64)
    to_schema = fn("to_arrow_schema", ptr, ptr, C.POINTER(ptr), C.POINTER(C.c_char_p), u64, C.POINTER(Schema))
    error_message = fn("error_data_message", C.c_char_p, ptr)
    to_array = fn("result_arrow_array", None, Result, ptr, C.POINTER(C.POINTER(Array)))
    fetch = fn("fetch_chunk", ptr, Result)
    query_arrow = fn("query_arrow", C.c_int, ptr, C.c_char_p, C.POINTER(ptr))
    prepared_arrow = fn("execute_prepared_arrow", C.c_int, ptr, C.POINTER(ptr))
    arrow_schema = fn("query_arrow_schema", C.c_int, ptr, C.POINTER(C.POINTER(Schema)))
    destroyers = {kind: fn("destroy_" + kind, None, C.POINTER(ptr)) for kind in (
        "arrow_options", "logical_type", "data_chunk", "prepare", "pending", "arrow"
    )}
    db, con = ptr(), ptr()
    assert open_db(None, ref(db)) == connect(db, ref(con)) == 0

    def set_sql(sql):
        result = Result()
        assert query(con, sql.encode(), ref(result)) == 0, sql
        destroy(ref(result))

    profiles = {
        "regular": "SET TimeZone='UTC'; SET arrow_large_buffer_size=false; SET arrow_output_version='1.0'; SET produce_arrow_string_view=false; SET arrow_output_list_view=false; SET arrow_lossless_conversion=false",
        "large": "SET TimeZone='America/New_York'; SET arrow_large_buffer_size=true; SET arrow_output_version='1.5'; SET produce_arrow_string_view=false; SET arrow_output_list_view=false; SET arrow_lossless_conversion=true",
        "views": "SET TimeZone='Asia/Tokyo'; SET arrow_large_buffer_size=false; SET arrow_output_version='1.4'; SET produce_arrow_string_view=true; SET arrow_output_list_view=true; SET arrow_lossless_conversion=false",
    }
    sql = b"SELECT 'hello'::VARCHAR s, ['a','bb']::VARCHAR[] l, 1.25::DECIMAL(6,2) d, TIMESTAMPTZ '2024-01-01 12:00:00+00' t, UUID '123e4567-e89b-12d3-a456-426614174000' u"
    names = (C.c_char_p * 5)(b"s", b"l", b"d", b"t", b"u")
    report = []
    assert not options(None)
    for profile, settings in profiles.items():
        for mode in ("query", "prepared", "stream", "pending", "arrow", "prepared_arrow", "error"):
            set_sql(settings)
            result, statement, pending_result, legacy = Result(), ptr(), ptr(), ptr()
            if mode in ("prepared", "stream", "pending", "prepared_arrow"):
                assert prepare(con, sql, ref(statement)) == 0
            if mode == "query":
                assert query(con, sql, ref(result)) == 0
            elif mode == "prepared":
                assert execute(statement, ref(result)) == 0
            elif mode == "stream":
                assert stream(statement, ref(result)) == 0
            elif mode == "pending":
                assert pending(statement, ref(pending_result)) == 0
                assert execute_pending(pending_result, ref(result)) == 0
            elif mode == "arrow":
                assert query_arrow(con, sql, ref(legacy)) == 0
            elif mode == "prepared_arrow":
                assert prepared_arrow(statement, ref(legacy)) == 0
            else:
                assert query(con, b"SELECT no_such_column", ref(result)) == 1

            if mode == "error":
                # Failed C++ results have no client context. Schema conversion with
                # their options raises INTERNAL; only verify the public handle life.
                before = ptr(options(ref(result)))
                set_sql(profiles["views" if profile != "views" else "large"])
                after = ptr(options(ref(result)))
                assert before and after
                destroy(ref(result))
                for value in (before, after):
                    destroyers["arrow_options"](ref(value))
                report.append({"profile": profile, "mode": mode, "owned_options": True})
                continue

            # Fetch a stream's chunk before another query invalidates the stream.
            chunk = ptr(fetch(result)) if mode not in ("arrow", "prepared_arrow", "error") else ptr()
            types = (ptr * 5)()
            if mode not in ("arrow", "prepared_arrow", "error"):
                for i in range(5):
                    types[i] = logical(ref(result), i)
            before = ptr(options(ref(result))) if not legacy else ptr()
            set_sql(profiles["views" if profile != "views" else "large"])
            schema = Schema()
            if legacy:
                target = C.pointer(schema)
                assert arrow_schema(legacy, ref(target)) == 0
                entry = {"profile": profile, "mode": mode, "schema": schema_json(schema)}
            else:
                after = ptr(options(ref(result)))
                current = ptr()
                current_options(con, ref(current))
                schemas = []
                for value in (before, after, current):
                    temp = Schema()
                    error = to_schema(value, types, names, 5, ref(temp))
                    assert not error, (profile, mode, error_message(error))
                    schemas.append(schema_json(temp))
                    release(temp)
                entry = {"profile": profile, "mode": mode, "before": schemas[0],
                         "after": schemas[1], "current": schemas[2]}
                if chunk:
                    array = Array()
                    target = C.pointer(array)
                    to_array(result, chunk, ref(target))
                    assert array.release and array.length == 1
                    string = array.children[0].contents
                    # Read only two 32-bit words, safe for all three layouts.
                    entry["string_buffers"] = string.n_buffers
                    entry["string_buffer_words"] = list((C.c_int32 * 2).from_address(string.buffers[1]))
                    release(array)
                # Options have independent ownership after their result is destroyed.
                destroy(ref(result))
                temp = Schema()
                assert not to_schema(after, types, names, 5, ref(temp))
                entry["after_destroy"] = schema_json(temp)
                release(temp)
                for value in (before, after, current):
                    destroyers["arrow_options"](ref(value))
            release(schema)
            for value in types:
                if value:
                    value = ptr(value)
                    destroyers["logical_type"](ref(value))
            for kind, value in (("data_chunk", chunk), ("prepare", statement),
                                ("pending", pending_result), ("arrow", legacy)):
                if value:
                    destroyers[kind](ref(value))
            report.append(entry)
    fn("disconnect", None, C.POINTER(ptr))(ref(con))
    fn("close", None, C.POINTER(ptr))(ref(db))
    print(json.dumps(report, indent=2, sort_keys=True))
    if args.reference:
        expected = json.loads(args.reference.read_text())
        mismatches = [f"{item['profile']}/{item['mode']}" for i, item in enumerate(report)
                      if i >= len(expected) or item != expected[i]]
        assert len(expected) == len(report), "Reference case count differs"
        if mismatches:
            print("Arrow snapshot mismatches: " + ", ".join(mismatches), file=sys.stderr)
            raise SystemExit(1)


if __name__ == "__main__":
    main()
