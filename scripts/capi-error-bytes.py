#!/usr/bin/env python3
"""Compare scalar callback and SQL errors through the C API, without text decoding.

Run each library in its own process so its symbols and database state stay isolated.
Libraries must be built and signed before invoking this script.
"""

import argparse
import ctypes as c
import hashlib
import json
from pathlib import Path
import subprocess
import sys


class Result(c.Structure):
    _fields_ = [
        ("deprecated_column_count", c.c_uint64),
        ("deprecated_row_count", c.c_uint64),
        ("deprecated_rows_changed", c.c_uint64),
        ("deprecated_columns", c.c_void_p),
        ("deprecated_error_message", c.c_void_p),
        ("internal_data", c.c_void_p),
    ]


def collect(library):
    lib = c.CDLL(str(library.resolve()))
    handle = c.c_void_p
    pointer = c.POINTER(handle)
    result_pointer = c.POINTER(Result)
    execute_callback = c.CFUNCTYPE(None, handle, handle, handle)
    state_callback = c.CFUNCTYPE(None, handle)
    signatures = [
        ("open", [c.c_char_p, pointer], c.c_int),
        ("connect", [handle, pointer], c.c_int),
        ("disconnect", [pointer], None),
        ("close", [pointer], None),
        ("create_scalar_function", [], handle),
        ("destroy_scalar_function", [pointer], None),
        ("scalar_function_set_name", [handle, c.c_char_p], None),
        ("scalar_function_set_return_type", [handle, handle], None),
        ("scalar_function_set_function", [handle, execute_callback], None),
        ("scalar_function_set_bind", [handle, state_callback], None),
        ("scalar_function_set_init", [handle, state_callback], None),
        ("scalar_function_set_volatile", [handle], None),
        ("scalar_function_set_error", [handle, c.c_char_p], None),
        ("scalar_function_bind_set_error", [handle, c.c_char_p], None),
        ("scalar_function_init_set_error", [handle, c.c_char_p], None),
        ("unsafe_vector_assign_string_element_len", [handle, c.c_uint64, c.c_char_p, c.c_uint64], None),
        ("register_scalar_function", [handle, handle], c.c_int),
        ("create_logical_type", [c.c_int], handle),
        ("destroy_logical_type", [pointer], None),
        ("query", [handle, c.c_char_p, result_pointer], c.c_int),
        ("result_error", [result_pointer], c.c_char_p),
        ("result_error_type", [result_pointer], c.c_int),
        ("destroy_result", [result_pointer], None),
        ("prepare", [handle, c.c_char_p, pointer], c.c_int),
        ("prepare_error", [handle], c.c_char_p),
        ("execute_prepared", [handle, result_pointer], c.c_int),
        ("destroy_prepare", [pointer], None),
    ]
    for name, args, result_type in signatures:
        function = getattr(lib, "duckdb_" + name)
        function.argtypes, function.restype = args, result_type

    db, connection = handle(), handle()
    assert lib.duckdb_open(None, c.byref(db)) == 0
    assert lib.duckdb_connect(db, c.byref(connection)) == 0
    phase, payload = "", b""

    @state_callback
    def bind(info):
        if phase == "bind":
            lib.duckdb_scalar_function_bind_set_error(info, payload)

    @state_callback
    def init(info):
        if phase == "init":
            lib.duckdb_scalar_function_init_set_error(info, payload)

    @execute_callback
    def execute(info, _input, _output):
        lib.duckdb_scalar_function_set_error(info, payload)

    @execute_callback
    def value(_info, _input, output):
        # The exported unsafe assignment API deliberately bypasses VARCHAR validation.
        # Feed those raw vector bytes to error(), including embedded NULs.
        lib.duckdb_unsafe_vector_assign_string_element_len(output, 0, payload, len(payload))

    function = handle(lib.duckdb_create_scalar_function())
    integer_type = handle(lib.duckdb_create_logical_type(4))
    lib.duckdb_scalar_function_set_name(function, b"byte_error")
    lib.duckdb_scalar_function_set_return_type(function, integer_type)
    lib.duckdb_scalar_function_set_function(function, execute)
    lib.duckdb_scalar_function_set_bind(function, bind)
    lib.duckdb_scalar_function_set_init(function, init)
    lib.duckdb_scalar_function_set_volatile(function)
    assert lib.duckdb_register_scalar_function(connection, function) == 0
    value_function = handle(lib.duckdb_create_scalar_function())
    varchar_type = handle(lib.duckdb_create_logical_type(17))
    lib.duckdb_scalar_function_set_name(value_function, b"byte_value")
    lib.duckdb_scalar_function_set_return_type(value_function, varchar_type)
    lib.duckdb_scalar_function_set_function(value_function, value)
    lib.duckdb_scalar_function_set_volatile(value_function)
    assert lib.duckdb_register_scalar_function(connection, value_function) == 0
    records = []

    def query(sql):
        result = Result()
        status = lib.duckdb_query(connection, sql, c.byref(result))
        record = [status, lib.duckdb_result_error_type(c.byref(result)),
                  (lib.duckdb_result_error(c.byref(result)) or b"").hex()]
        lib.duckdb_destroy_result(c.byref(result))
        return record

    samples = [bytes([i]) for i in range(256)] + [
        b"", b"caf\xe9", "café 😀".encode(), b"a\0b", b"\xff\xfe\x80",
        b'quote"\\\n\r\t\x08\x0c\x1f', b'{"message":"already json"}',
        b"\xc0\x80", b"\xed\xa0\x80", b"\xf4\x90\x80\x80",
    ]
    try:
        for json_errors in (False, True):
            assert query(b"SET errors_as_json=" + str(json_errors).lower().encode())[0] == 0
            for phase in ("bind", "init", "execute", "sql"):
                for payload in samples:
                    sql = b"SELECT error(byte_value())" if phase == "sql" else b"SELECT byte_error()"
                    direct = query(sql)
                    assert direct[0] == 1, (phase, payload, direct)
                    prepared = handle()
                    status = lib.duckdb_prepare(connection, sql, c.byref(prepared))
                    prepare_error = (lib.duckdb_prepare_error(prepared) or b"").hex()
                    execution = None
                    if status == 0:
                        result = Result()
                        execution = [lib.duckdb_execute_prepared(prepared, c.byref(result)),
                                     lib.duckdb_result_error_type(c.byref(result)),
                                     (lib.duckdb_result_error(c.byref(result)) or b"").hex()]
                        lib.duckdb_destroy_result(c.byref(result))
                        assert execution[0] == 1, (phase, payload, execution)
                    else:
                        assert phase == "bind", (phase, payload, status)
                    lib.duckdb_destroy_prepare(c.byref(prepared))
                    records.append({"json": json_errors, "phase": phase,
                                    "payload": payload.hex(), "query": direct,
                                    "prepare": [status, prepare_error], "execute": execution})
        assert query(b"SELECT 42")[0] == 0  # Connection survives all callback failures.
    finally:
        lib.duckdb_destroy_logical_type(c.byref(integer_type))
        lib.duckdb_destroy_logical_type(c.byref(varchar_type))
        lib.duckdb_destroy_scalar_function(c.byref(function))
        lib.duckdb_destroy_scalar_function(c.byref(value_function))
        lib.duckdb_disconnect(c.byref(connection))
        lib.duckdb_close(c.byref(db))
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--cpp", type=Path)
    parser.add_argument("--rust", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.library:
        print(json.dumps(collect(args.library)))
        return 0
    if not all((args.cpp, args.rust, args.output)):
        parser.error("--cpp, --rust and --output are required")
    args.output.mkdir(parents=True, exist_ok=True)
    records, hashes = {}, {}
    for name, path in (("cpp", args.cpp), ("rust", args.rust)):
        digest = hashlib.sha256()
        with path.open("rb") as library:
            for block in iter(lambda: library.read(1024 * 1024), b""):
                digest.update(block)
        hashes[name] = digest.hexdigest()
        run = subprocess.run([sys.executable, __file__, "--library", str(path.resolve())],
                             check=True, capture_output=True, timeout=300)
        (args.output / (name + ".json")).write_bytes(run.stdout)
        records[name] = json.loads(run.stdout)
    assert len(records["cpp"]) == len(records["rust"]) == 2128
    mismatches = [{"cpp": a, "rust": b} for a, b in zip(records["cpp"], records["rust"]) if a != b]
    report = {"records_per_library": len(records["cpp"]), "library_sha256": hashes,
              "mismatch_count": len(mismatches), "mismatches": mismatches}
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"{len(records['cpp'])} callback cases, {len(mismatches)} mismatches")
    return bool(mismatches)


if __name__ == "__main__":
    sys.exit(main())
