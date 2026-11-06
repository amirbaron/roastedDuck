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


class CallbackAPI:
    """Own a database and callback definitions for one non-scalar callback family."""

    handle = c.c_void_p
    state_callback = c.CFUNCTYPE(None, handle)
    pair_callback = c.CFUNCTYPE(None, handle, handle)

    def __init__(self, library):
        self.lib = c.CDLL(str(library.resolve()))
        self.callbacks = []
        self.definitions = []
        self.phase, self.payload, self.hits, self.modes = "", b"", 0, []
        h, p, rp = self.handle, c.POINTER(self.handle), c.POINTER(Result)
        for name, args, result in [
            ("open", [c.c_char_p, p], c.c_int), ("connect", [h, p], c.c_int),
            ("disconnect", [p], None), ("close", [p], None),
            ("query", [h, c.c_char_p, rp], c.c_int),
            ("result_error", [rp], c.c_char_p), ("result_error_type", [rp], c.c_int),
            ("destroy_result", [rp], None),
            ("prepare", [h, c.c_char_p, p], c.c_int),
            ("prepare_error", [h], c.c_char_p),
            ("execute_prepared", [h, rp], c.c_int), ("destroy_prepare", [p], None),
            ("create_logical_type", [c.c_int], h), ("destroy_logical_type", [p], None),
        ]:
            self.declare(name, args, result)
        self.db, self.connection = h(), h()
        assert self.lib.duckdb_open(None, c.byref(self.db)) == 0
        assert self.lib.duckdb_connect(self.db, c.byref(self.connection)) == 0
        self.integer = h(self.lib.duckdb_create_logical_type(5))  # BIGINT
        assert self.query(b"SET threads=1")[0] == 0

    def declare(self, name, args, result=None):
        function = getattr(self.lib, "duckdb_" + name)
        function.argtypes, function.restype = args, result
        return function

    def callback(self, callback_type, function):
        result = callback_type(function)
        self.callbacks.append(result)  # Keep native function pointers alive until close.
        return result

    def definition(self, family, name):
        h, p = self.handle, c.POINTER(self.handle)
        create = self.declare("create_" + family + "_function", [], h)
        destroy = self.declare("destroy_" + family + "_function", [p])
        definition = h(create())
        self.definitions.append((destroy, definition))
        self.declare(family + "_function_set_name", [h, c.c_char_p])(definition, name)
        return definition

    def report(self, phase, setter, info):
        if self.phase.removesuffix("_empty").removesuffix("_pruned").removesuffix("_prepare").removesuffix("_window") == phase:
            self.hits += 1
            setter(info, self.payload)

    def query(self, sql):
        result = Result()
        status = self.lib.duckdb_query(self.connection, sql, c.byref(result))
        record = self.result_record(status, result)
        self.lib.duckdb_destroy_result(c.byref(result))
        return record

    def result_record(self, status, result):
        return [status, self.lib.duckdb_result_error_type(c.byref(result)),
                (self.lib.duckdb_result_error(c.byref(result)) or b"").hex()]

    def close(self):
        for destroy, definition in reversed(self.definitions):
            destroy(c.byref(definition))
        self.lib.duckdb_destroy_logical_type(c.byref(self.integer))
        self.lib.duckdb_disconnect(c.byref(self.connection))
        self.lib.duckdb_close(c.byref(self.db))

    def table(self):
        h = self.handle
        definition = self.definition("table", b"byte_table")
        add_column = self.declare("bind_add_result_column", [h, c.c_char_p, h])
        bind_error = self.declare("bind_set_error", [h, c.c_char_p])
        init_error = self.declare("init_set_error", [h, c.c_char_p])
        execute_error = self.declare("function_set_error", [h, c.c_char_p])

        def bind(info):
            add_column(info, b"i", self.integer)
            self.report("bind", bind_error, info)

        for phase, function in [
            ("bind", bind),
            ("init", lambda info: self.report("init", init_error, info)),
            ("local_init", lambda info: self.report("local_init", init_error, info)),
        ]:
            callback = self.callback(self.state_callback, function)
            self.declare("table_function_set_" + phase, [h, self.state_callback])(definition, callback)
        execute = self.callback(self.pair_callback,
                                lambda info, output: self.report("execute", execute_error, info))
        self.declare("table_function_set_function", [h, self.pair_callback])(definition, execute)
        assert self.declare("register_table_function", [h, h], c.c_int)(self.connection, definition) == 0
        return ("bind", "init", "local_init", "execute"), lambda phase: b"SELECT * FROM byte_table()"

    def aggregate(self):
        h, u = self.handle, c.c_uint64
        definition = self.definition("aggregate", b"byte_aggregate")
        self.declare("aggregate_function_add_parameter", [h, h])(definition, self.integer)
        self.declare("aggregate_function_set_return_type", [h, h])(definition, self.integer)
        error = self.declare("aggregate_function_set_error", [h, c.c_char_p])
        data = self.declare("vector_get_data", [h], h)
        size_type = c.CFUNCTYPE(u, h)
        update_type = c.CFUNCTYPE(None, h, h, c.POINTER(h))
        combine_type = c.CFUNCTYPE(None, h, c.POINTER(h), c.POINTER(h), u)
        finalize_type = c.CFUNCTYPE(None, h, c.POINTER(h), h, u, u)

        def size(info):
            self.report("size", error, info)
            return 8

        def init(info, state):
            c.cast(state, c.POINTER(c.c_int64))[0] = 0
            self.report("init", error, info)

        def finalize(info, states, output, count, offset):
            values = c.cast(data(output), c.POINTER(c.c_int64))
            for index in range(count):
                values[index + offset] = 0
            self.report("finalize", error, info)

        callbacks = [
            self.callback(size_type, size),
            self.callback(self.pair_callback, init),
            self.callback(update_type, lambda info, chunk, states: self.report("update", error, info)),
            self.callback(combine_type,
                          lambda info, source, target, count: self.report("combine", error, info)),
            self.callback(finalize_type, finalize),
        ]
        self.declare("aggregate_function_set_functions",
                     [h, size_type, self.pair_callback, update_type, combine_type, finalize_type])(
                         definition, *callbacks)
        assert self.declare("register_aggregate_function", [h, h], c.c_int)(self.connection, definition) == 0
        assert self.query(b"SET threads=2")[0] == 0
        phases = ("size", "init", "update", "combine", "finalize", "size_empty", "init_empty",
                  "size_pruned", "size_prepare", "size_window")
        def query(phase):
            sql = b"SELECT byte_aggregate(i) FROM range(" + (
                b"0" if phase.endswith("_empty") else b"100000" if phase == "combine" else b"100") + b") r(i)"
            if phase.endswith("_pruned"):
                return sql + b" LIMIT 0"
            if phase.endswith("_prepare"):
                return b"PREPARE byte_stmt AS " + sql
            if phase.endswith("_window"):
                return b"SELECT byte_aggregate(i) OVER () FROM range(100) r(i)"
            return sql
        return phases, query

    def copy(self):
        h = self.handle
        definition = self.definition("copy", b"byte_copy")
        for phase in ("bind", "global_init", "sink", "finalize"):
            setter = self.declare("copy_function_" + phase + "_set_error", [h, c.c_char_p])
            callback_type = self.pair_callback if phase == "sink" else self.state_callback
            def function(info, *unused, phase=phase, setter=setter):
                self.report(phase, setter, info)
            callback = self.callback(callback_type, function)
            self.declare("copy_function_set_" + phase, [h, callback_type])(definition, callback)
        assert self.declare("register_copy_function", [h, h], c.c_int)(self.connection, definition) == 0
        return ("bind", "global_init", "sink", "finalize"), lambda phase: (
            b"COPY (SELECT 1::BIGINT) TO 'byte-error-unused-output' (FORMAT byte_copy)")

    def replacement(self):
        h = self.handle
        callback_type = c.CFUNCTYPE(None, h, c.c_char_p, h)
        error = self.declare("replacement_scan_set_error", [h, c.c_char_p])
        name = self.declare("replacement_scan_set_function_name", [h, c.c_char_p])
        add = self.declare("replacement_scan_add_parameter", [h, h])
        create = self.declare("create_int64", [c.c_int64], h)
        destroy = self.declare("destroy_value", [c.POINTER(h)])

        def replacement(info, table, extra):
            if table == b"byte_missing_table":
                self.hits += 1
                error(info, self.payload)
                # Empty errors are ignored; provide a real replacement for that case.
                name(info, b"range")
                parameter = h(create(1))
                add(info, parameter)
                destroy(c.byref(parameter))

        callback = self.callback(callback_type, replacement)
        self.declare("add_replacement_scan", [h, callback_type, h, h])(
            self.db, callback, None, None)
        return ("replacement",), lambda phase: (
            b"SELECT CASE WHEN count(*)=1 THEN TRUE ELSE error('bad replacement') END "
            b"FROM byte_missing_table")

    def cast(self):
        h, u = self.handle, c.c_uint64
        alias = self.declare("logical_type_set_alias", [h, c.c_char_p])
        register_type = self.declare("register_logical_type", [h, h, h], c.c_int)
        source = h(self.lib.duckdb_create_logical_type(17))  # VARCHAR
        alias(source, b"byte_source")
        alias(self.integer, b"byte_target")
        assert register_type(self.connection, source, None) == 0
        assert register_type(self.connection, self.integer, None) == 0
        create = self.declare("create_cast_function", [], h)
        definition = h(create())
        self.definitions.append((self.declare("destroy_cast_function", [c.POINTER(h)]), definition))
        self.declare("cast_function_set_source_type", [h, h])(definition, source)
        self.declare("cast_function_set_target_type", [h, h])(definition, self.integer)
        self.lib.duckdb_destroy_logical_type(c.byref(source))
        callback_type = c.CFUNCTYPE(c.c_bool, h, u, h, h)
        error = self.declare("cast_function_set_row_error", [h, c.c_char_p, u, h])
        mode = self.declare("cast_function_get_cast_mode", [h], c.c_int)

        def cast(info, count, source, output):
            self.hits += 1
            self.modes.append(mode(info))
            for row in range(count):
                error(info, self.payload, row, output)
            return False

        callback = self.callback(callback_type, cast)
        self.declare("cast_function_set_function", [h, callback_type])(definition, callback)
        assert self.declare("register_cast_function", [h, h], c.c_int)(self.connection, definition) == 0
        assert self.query(b"CREATE TABLE byte_cast_inputs(v byte_source)")[0] == 0
        assert self.query(b"INSERT INTO byte_cast_inputs VALUES ('bad')")[0] == 0
        queries = {
            "constant": b"SELECT CAST('bad'::byte_source AS byte_target)",
            "column": b"SELECT CAST(v AS byte_target) FROM byte_cast_inputs",
            "try": b"SELECT CASE WHEN TRY_CAST(v AS byte_target) IS NULL THEN TRUE "
                   b"ELSE error('TRY_CAST result was not NULL') END FROM byte_cast_inputs",
        }
        return tuple(queries), queries.__getitem__


def collect_callbacks(library, family):
    api = CallbackAPI(library)
    samples = [bytes([i]) for i in range(256)] + [
        b"", b"caf\xe9", "café 😀".encode(), b"a\0b", b"\xff\xfe\x80",
        b'quote"\\\n\r\t\x08\x0c\x1f', b'{"message":"already json"}',
        b"\xc0\x80", b"\xed\xa0\x80", b"\xf4\x90\x80\x80",
    ]
    records = []
    try:
        phases, sql_for_phase = getattr(api, family)()
        for json_errors in (False, True):
            assert api.query(b"SET errors_as_json=" + str(json_errors).lower().encode())[0] == 0
            for api.phase in phases:
                for api.payload in samples:
                    sql = sql_for_phase(api.phase)
                    expected_error = not (
                        (family == "cast" and api.phase == "try") or
                        (family == "aggregate" and api.phase == "size_pruned") or
                        (family == "replacement" and not api.payload.split(b"\0", 1)[0]))
                    api.hits, api.modes = 0, []
                    direct = api.query(sql)
                    assert direct[0] == int(expected_error), (family, api.phase, api.payload, direct)
                    if family == "aggregate" and api.phase == "size_pruned":
                        assert not api.hits, ("pruned callback was invoked", family, api.phase)
                    else:
                        assert api.hits, ("callback not exercised", family, api.phase)
                    prepared = api.handle()
                    status = api.lib.duckdb_prepare(api.connection, sql, c.byref(prepared))
                    prepare_error = (api.lib.duckdb_prepare_error(prepared) or b"").hex()
                    execution = None
                    if status == 0:
                        result = Result()
                        execution = api.result_record(
                            api.lib.duckdb_execute_prepared(prepared, c.byref(result)), result)
                        api.lib.duckdb_destroy_result(c.byref(result))
                        assert execution[0] == int(expected_error), (
                            family, api.phase, api.payload, execution)
                    else:
                        assert expected_error, (family, api.phase, api.payload, status)
                    api.lib.duckdb_destroy_prepare(c.byref(prepared))
                    records.append({"family": family, "json": json_errors, "phase": api.phase,
                                    "payload": api.payload.hex(), "query": direct,
                                    "prepare": [status, prepare_error], "execute": execution,
                                    "cast_modes": sorted(set(api.modes))})
        assert api.query(b"SELECT 42")[0] == 0
    finally:
        api.close()
    return records


FAMILY_COUNTS = {"scalar": 2128, "table": 2128, "aggregate": 5320,
                 "copy": 2128, "replacement": 532, "cast": 1596}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--cpp", type=Path)
    parser.add_argument("--rust", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--family", choices=[*FAMILY_COUNTS, "all"], default="scalar")
    args = parser.parse_args()
    if args.library:
        families = FAMILY_COUNTS if args.family == "all" else [args.family]
        records = []
        for family in families:
            records.extend(collect(args.library) if family == "scalar" else
                           collect_callbacks(args.library, family))
        print(json.dumps(records))
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
        run = subprocess.run([sys.executable, str(Path(__file__).resolve()),
                              "--library", str(path.resolve()), "--family", args.family],
                             capture_output=True, timeout=300,
                             cwd=args.output.resolve())
        (args.output / (name + ".json")).write_bytes(run.stdout)
        (args.output / (name + ".stderr.log")).write_bytes(run.stderr)
        run.check_returncode()
        records[name] = json.loads(run.stdout)
    expected = sum(FAMILY_COUNTS.values()) if args.family == "all" else FAMILY_COUNTS[args.family]
    assert len(records["cpp"]) == len(records["rust"]) == expected
    mismatches = [{"cpp": a, "rust": b} for a, b in zip(records["cpp"], records["rust"]) if a != b]
    report = {"records_per_library": len(records["cpp"]), "library_sha256": hashes,
              "mismatch_count": len(mismatches), "mismatches": mismatches}
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"{len(records['cpp'])} callback cases, {len(mismatches)} mismatches")
    return bool(mismatches)


if __name__ == "__main__":
    sys.exit(main())
