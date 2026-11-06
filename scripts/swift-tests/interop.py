"""Run both directions of checkpoint/WAL handoff between Swift and frozen C++."""

import argparse
import ctypes as C
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

from prepare import digest


def cpp_worker(library, directory, mode):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "capi-tests"))
    from arrow_snapshot import Result
    ptr = C.c_void_p
    lib = C.CDLL(str(library))

    def fn(name, result, *args):
        function = getattr(lib, "duckdb_" + name)
        function.restype, function.argtypes = result, list(args)
        return function

    class Info(C.Structure):
        _fields_ = [("image", C.c_char_p), ("base", ptr), ("symbol", C.c_char_p), ("address", ptr)]
    system = C.CDLL("/usr/lib/libSystem.B.dylib")
    system.dladdr.argtypes, system.dladdr.restype = [ptr, C.POINTER(Info)], C.c_int
    info = Info()
    assert system.dladdr(C.cast(lib.duckdb_library_version, ptr), C.byref(info))
    assert Path(os.fsdecode(info.image)).resolve() == library
    db, connection = ptr(), ptr()
    assert fn("open", C.c_int, C.c_char_p, C.POINTER(ptr))(
        os.fsencode(directory / "client.duckdb"), C.byref(db)) == 0
    assert fn("connect", C.c_int, ptr, C.POINTER(ptr))(db, C.byref(connection)) == 0

    def query(sql, collect=False):
        result = Result()
        try:
            state = fn("query", C.c_int, ptr, C.c_char_p, C.POINTER(Result))(
                connection, sql.encode(), C.byref(result))
            error = fn("result_error", C.c_char_p, C.POINTER(Result))(C.byref(result))
            assert state == 0, error
            if collect:
                rows = []
                for row in range(fn("row_count", C.c_uint64, C.POINTER(Result))(C.byref(result))):
                    value = fn("value_varchar", ptr, C.POINTER(Result), C.c_uint64, C.c_uint64)(
                        C.byref(result), 1, row)
                    label = C.string_at(value).decode() if value else None
                    if value:
                        fn("free", None, ptr)(value)
                    rows.append([fn("value_int64", C.c_int64, C.POINTER(Result), C.c_uint64, C.c_uint64)(
                        C.byref(result), 0, row), label])
                return rows
        finally:
            fn("destroy_result", None, C.POINTER(Result))(C.byref(result))

    expected = [[1, "prépared שלום"], [2, "appended"], [3, None], [4, "wal"]]
    if mode.endswith("writer"):
        query("CREATE TABLE items(id BIGINT, label VARCHAR); "
              "INSERT INTO items VALUES (1, 'prépared שלום'), (2, 'appended'), (3, NULL); CHECKPOINT; "
              "SET wal_autocheckpoint='1GB'; INSERT INTO items VALUES (4, 'wal')")
        if mode == "wal-writer":
            assert (directory / "client.duckdb.wal").stat().st_size > 0
            (directory / "writer-ready").write_text("ready")
            sys.stdin.readline()
            raise AssertionError("writer must be stopped by harness")
        query("CHECKPOINT")
    elif mode == "read-write":
        assert query("SELECT * FROM items ORDER BY id", True) == expected
        query("INSERT INTO items VALUES (5, 'handoff'); CHECKPOINT")
        expected.append([5, "handoff"])
    else:
        expected.append([5, "handoff"])
    rows = query("SELECT * FROM items ORDER BY id", True)
    assert rows == expected, rows
    fn("disconnect", None, C.POINTER(ptr))(C.byref(connection))
    fn("close", None, C.POINTER(ptr))(C.byref(db))
    report = {"rows": rows, "function_image": os.fsdecode(info.image),
              "version": fn("library_version", C.c_char_p)().decode()}
    (directory / "cpp.json").write_text(json.dumps(report, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--consumer", type=Path)
    parser.add_argument("--cpp-library", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--worker", choices=["wal-writer", "checkpoint-writer", "read-write", "read-final"])
    args = parser.parse_args()
    library, out = args.cpp_library.resolve(), args.out.resolve()
    if args.worker:
        return cpp_worker(library, out, args.worker)
    if out.exists():
        parser.error("output must be new")
    consumer = args.consumer.resolve()
    for binary in [library, consumer]:
        subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
    hashes = {str(binary): digest(binary) for binary in [library, consumer]}
    out.mkdir(parents=True)
    env = dict(os.environ)
    for name in ["tmp", "foundation-home"]:
        (out / name).mkdir()
    env.update(TMPDIR=str(out / "tmp"), CFFIXED_USER_HOME=str(out / "foundation-home"))

    def command(engine, path, mode):
        if engine == "rust":
            return ["nice", "-n", "10", str(consumer), str(path), mode]
        return ["nice", "-n", "10", sys.executable, str(Path(__file__).resolve()),
                "--cpp-library", str(library), "--out", str(path), "--worker", mode]

    cases = []
    for writer, reader in [("rust", "cpp"), ("cpp", "rust")]:
        for kind in ["checkpoint", "wal"]:
            case = out / (writer + "-" + kind)
            case.mkdir()
            (case / "writer").mkdir()
            with (case / "writer.log").open("x") as log:
                process = subprocess.Popen(command(writer, case / "writer", kind + "-writer"), env=env,
                                           stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)
                try:
                    if kind == "wal":
                        deadline = time.monotonic() + 30
                        while not (case / "writer/writer-ready").exists():
                            assert process.poll() is None, "writer terminated before WAL readiness"
                            assert time.monotonic() < deadline, "WAL readiness timeout"
                            time.sleep(0.1)
                    else:
                        assert process.wait(timeout=30) == 0
                    snapshot = case / "before-replay"
                    snapshot.mkdir()
                    files = []
                    for file in sorted((case / "writer").glob("client.duckdb*")):
                        shutil.copy2(file, snapshot / file.name)
                        files.append({"name": file.name, "bytes": file.stat().st_size, "sha256": digest(file)})
                    if kind == "wal":
                        assert any(item["name"].endswith(".wal") and item["bytes"] > 0 for item in files)
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=10)
                    process.stdin.close()
            active = case / "handoff"
            shutil.copytree(snapshot, active)
            reports = []
            for engine, mode in [(reader, "read-write"), (writer, "read-final")]:
                report_name = "consumer.json" if engine == "rust" else "cpp.json"
                with (case / (engine + "-" + mode + ".log")).open("x") as log:
                    result = subprocess.run(command(engine, active, mode), env=env, stdout=log,
                                            stderr=subprocess.STDOUT, timeout=30)
                assert result.returncode == 0, (engine, mode, result.returncode)
                reports.append(json.loads((active / report_name).read_text()))
                shutil.copy2(active / report_name, case / (engine + "-" + mode + ".json"))
            swift = next(report for report in reports if "engine_images" in report)
            assert Path(swift["function_image"]).is_relative_to(consumer.parent)
            assert swift["version"] == "v1.5.6"
            cases.append({"writer": writer, "reader": reader, "kind": kind,
                          "writer_exit_code": process.returncode, "snapshots": files, "reports": reports})
    for binary, expected in hashes.items():
        assert digest(Path(binary)) == expected
    summary = {"cases": cases, "binary_hashes": hashes, "all_four_passed": True,
               "scope": "Two-column client handoff on frozen1.5.6 macOSarm64; broader storage/WAL gates open"}
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({"cases": len(cases), "all_four_passed": True, "out": str(out)}))


if __name__ == "__main__":
    main()
