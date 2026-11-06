"""Observe actual startup config acceptance and access-mode readback."""

import argparse
import ctypes as C
import hashlib
import itertools
import json
from pathlib import Path
import subprocess

from arrow_snapshot import Result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    library, out = args.library.resolve(), args.out.resolve()
    assert not out.exists()
    out.mkdir(parents=True)
    subprocess.run(["codesign", "--verify", "--strict", str(library)], check=True)
    original_hash = hashlib.sha256(library.read_bytes()).hexdigest()
    lib = C.CDLL(str(library))
    ptr = C.c_void_p

    def fn(name, result, *args):
        f = getattr(lib, "duckdb_" + name)
        f.restype, f.argtypes = result, list(args)
        return f

    class Info(C.Structure):
        _fields_ = [("image", C.c_char_p), ("base", ptr), ("symbol", C.c_char_p), ("address", ptr)]
    system = C.CDLL("/usr/lib/libSystem.B.dylib")
    system.dladdr.argtypes, system.dladdr.restype = [ptr, C.POINTER(Info)], C.c_int
    info = Info()
    assert system.dladdr(C.cast(lib.duckdb_open, ptr), C.byref(info))
    assert Path(info.image.decode()).resolve() == library
    (out / "manifest.json").write_text(json.dumps({"library": str(library),
        "library_sha256": original_hash, "function_image": info.image.decode()}, indent=2) + "\n")

    def read(db):
        connection, result = ptr(), Result()
        state = fn("connect", C.c_int, ptr, C.POINTER(ptr))(db, C.byref(connection))
        if state:
            return {"connect_state": state}
        try:
            assert fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))(
                connection, b"SELECT current_setting('access_mode'),current_setting('TimeZone'),current_setting('Calendar')", C.byref(result)) == 0
            row = {"connect_state": state}
            for index, name in enumerate(["access_mode", "timezone", "calendar"]):
                value = fn("value_varchar", ptr, C.POINTER(Result), C.c_uint64, C.c_uint64)(C.byref(result), index, 0)
                try:
                    row[name] = C.string_at(value).decode() if value else None
                finally:
                    if value:
                        fn("free", None, ptr)(value)
            return row
        finally:
            fn("destroy_result", None, C.POINTER(Result))(C.byref(result))
            fn("disconnect", None, C.POINTER(ptr))(C.byref(connection))

    file = out / "fixture.duckdb"
    db = ptr()
    assert fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))(str(file).encode(), C.byref(db)) == 0
    fn("close", None, C.POINTER(ptr))(C.byref(db))
    local = [("custom_profiling_settings", "{}"), ("debug_force_external", "true"),
        ("enable_caching_operators", "true"), ("enable_http_logging", "false"),
        ("enable_profiling", "json"), ("enable_progress_bar", "true"),
        ("enable_progress_bar_print", "false"), ("http_logging_output", ""),
        ("profile_output", str(out / "profile.json")), ("profiling_coverage", "ALL"),
        ("profiling_mode", "standard"), ("progress_bar_time", "1"), ("schema", "main"),
        ("search_path", "main"), ("streaming_buffer_size", "1MB"),
        ("profiling_output", str(out / "profile-alias.json")), ("SCHEMA", "main")]
    generic = [("default_order", "desc"), ("arrow_large_buffer_size", "true"),
               ("file_search_path", ""), ("unknown_option", "true")]
    extension = []
    for name, value in [("TimeZone", "Asia/Jerusalem"), ("Calendar", "buddhist")]:
        for autoload in ["true", "false"]:
            for order in ["flag-first", "value-first"]:
                options = [("autoload_known_extensions", autoload), (name, value)]
                if order == "value-first":
                    options.reverse()
                extension.append((f"extension:{name}:autoload={autoload}:{order}", options, None))
    for name, invalid, valid in [("TimeZone", "No/Such_Zone", "Asia/Jerusalem"),
                                 ("Calendar", "no_such_calendar", "buddhist")]:
        extension.append((f"extension:{name}:invalid", [(name, invalid)], None))
        for first, second, label in [(invalid, valid, "invalid-then-valid"),
                                      (valid, invalid, "valid-then-invalid")]:
            extension.append((f"extension:{name}:{label}",
                              [(name, first), (name.lower(), second)], None))
    for options in [[("TimeZone", "Asia/Jerusalem"), ("Calendar", "buddhist")],
                    [("Calendar", "buddhist"), ("TimeZone", "Asia/Jerusalem")]]:
        label = ":".join(name for name, _ in options)
        extension.append((f"extension:both:{label}:autoload=false",
                          [*options, ("autoload_known_extensions", "false")], None))
    for options in itertools.permutations([("TimeZone", "Asia/Jerusalem"),
                                           ("Calendar", "buddhist"),
                                           ("binary_as_string", "true")]):
        label = ":".join(name for name, _ in options)
        extension.append((f"extension:three:{label}:autoload=false",
                          [*options, ("autoload_known_extensions", "false")], None))
    rows = []
    for label, options, path in [("local:" + name, [(name, value)], None) for name, value in local] + [
            ("generic:" + name, [(name, value)], None) for name, value in generic] + [
            ("access:" + mode + ":" + store, [("access_mode", mode)], path)
            for mode in ["automatic", "read_write", "read_only"]
            for store, path in [("memory", None), ("file", str(file).encode())]] + extension:
        config, db, error = ptr(), ptr(), ptr()
        assert fn("create_config", C.c_int, C.POINTER(ptr))(C.byref(config)) == 0
        states = [fn("set_config", C.c_int, ptr, C.c_char_p, C.c_char_p)(config, name.encode(), value.encode())
                  for name, value in options]
        row = {"case": label, "set_states": states}
        if not any(states):
            state = fn("open_ext", C.c_int, C.c_char_p, C.POINTER(ptr), ptr, C.POINTER(ptr))(
                path, C.byref(db), config, C.byref(error))
            row.update(open_state=state, error=C.string_at(error).decode() if error else None)
            if not state:
                row.update(read(db))
            if error:
                fn("free", None, ptr)(error)
            fn("close", None, C.POINTER(ptr))(C.byref(db))
        fn("destroy_config", None, C.POINTER(ptr))(C.byref(config))
        rows.append(row)
        (out / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    (out / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    assert hashlib.sha256(library.read_bytes()).hexdigest() == original_hash
    print(json.dumps({"cases": len(rows), "results": str(out / "results.json")}))


if __name__ == "__main__":
    main()
