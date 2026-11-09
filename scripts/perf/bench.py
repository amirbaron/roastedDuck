"""Runs a benchmark suite on DuckDB 1.5.6 and roastedDuck; per query: best wall time of the
timed runs, its CPU time, the process's peak RSS, and a result check.

usage: python3 -I scripts/perf/bench.py SUITE [--threads N] [--reps 5] [--queries a,b]
       [--engines duck,rust] [--out results.json] [--no-check] [--memory-limit X] [--dump DIR]
SUITE: tpch1 | tpch3 | tpch10 (`tpchSF.duckdb`) | tpcds1 (`tpcdsSF.duckdb`) | h2og (`h2o_g.duckdb`)
       | h2oj (`h2o_j.duckdb`)
       | hits (`hits.duckdb`) | any `NAME.duckdb` with one or more `--sql QUERY`, in the data directory (`ROASTEDDUCK_PERF_DIR`, default
       `.context/perf`); `scripts/perf/data.sh` creates them with DuckDB's CLI.

Needs `cargo build --release --manifest-path crates/capi/Cargo.toml` (roastedDuck's `libduckdb`) and the reference tree
(`DUCKDB_SRC`, default `.context/duckdb-download/duckdb-1.5.6`) with its release build
(`build/release/duckdb`, `build/release/src/libduckdb.dylib`).

Each (engine, query) runs in its own Python process that loads the engine's libduckdb
(DuckDB's C API; roastedDuck's `crates/capi`) through ctypes, on the same database file: one
untimed warm-up run (whose result is checked), then REPS timed runs; peak RSS is the
process's `ru_maxrss`. h2oai queries are `CREATE TEMP TABLE ans AS ...` (dropped after
each run) and are checked with DuckDB's benchmark `RESULT_QUERY`.
"""
import argparse
import json
import math
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PERF = os.environ.get("ROASTEDDUCK_PERF_DIR") or os.environ.get("RUSTDB_PERF_DIR") or os.path.join(ROOT, ".context/perf")
REF = os.environ.get("DUCKDB_SRC", os.path.join(ROOT, ".context/duckdb-download/duckdb-1.5.6"))
LIBS = {"duck": os.path.join(REF, "build/release/src/libduckdb.dylib"),
        "rust": os.environ.get("RUST_LIB", os.path.join(ROOT, "target/release/libduckdb.dylib"))}
ADHOC = []
CLIS = {"duck": os.path.join(REF, "build/release/duckdb"), "rust": os.path.join(ROOT, "target/release/roastedDuck")}


def one_line(sql):
    lines = [re.sub(r"--.*$", "", line) for line in sql.splitlines()]
    return " ".join(line.strip() for line in lines if line.strip()).rstrip(";").strip()


def suite_queries(suite):
    """[(name, db, sql, check_sql or None)]"""
    out = []
    if suite.startswith("tpch") and not suite.endswith(".duckdb"):
        db = os.path.join(PERF, f"{suite}.duckdb")
        for i in range(1, 23):
            path = os.path.join(REF, "extension/tpch/dbgen/queries", f"q{i:02d}.sql")
            out.append((f"q{i:02d}", db, one_line(open(path).read()), None))
    elif suite.startswith("tpcds") and not suite.endswith(".duckdb"):
        db = os.path.join(PERF, f"{suite}.duckdb")
        for i in range(1, 100):
            path = os.path.join(REF, "extension/tpcds/dsdgen/queries", f"{i:02d}.sql")
            out.append((f"q{i:02d}", db, one_line(open(path).read()), None))
    elif suite in ("h2og", "h2oj"):
        kind = "group" if suite == "h2og" else "join"
        db = os.path.join(PERF, f"h2o_{suite[-1]}.duckdb")
        base = os.path.join(REF, "benchmark/h2oai", kind)
        for f in sorted(os.listdir(base)):
            m = re.match(r"q(\d+)\.benchmark$", f)
            if not m:
                continue
            text = open(os.path.join(base, f)).read()
            check = re.search(r"^RESULT_QUERY=(.*)$", text, re.M).group(1).strip().rstrip(";")
            sql = one_line(open(os.path.join(base, "queries", f"q{m.group(1)}.sql")).read())
            out.append((f"q{m.group(1)}", db, sql, check))
    elif suite.endswith(".duckdb"):
        # Ad-hoc: SUITE is a database in the data directory, the queries come from --sql.
        db = os.path.join(PERF, suite)
        for i, sql in enumerate(ADHOC):
            out.append((f"q{i + 1:02d}", db, one_line(sql), None))
    elif suite == "hits":
        db = os.path.join(PERF, "hits.duckdb")
        base = os.path.join(REF, "benchmark/clickbench/queries")
        for i in range(43):
            out.append((f"q{i:02d}", db, one_line(open(os.path.join(base, f"q{i:02d}.sql")).read()), None))
    else:
        sys.exit(f"unknown suite {suite}")
    return out


def child(engine, spec):
    """Runs inside the measured process: drives the engine through its C API (ctypes)."""
    import ctypes
    import resource
    import time

    lib = ctypes.CDLL(LIBS[engine])
    vp = ctypes.c_void_p

    class Result(ctypes.Structure):
        _fields_ = [("a", ctypes.c_uint64), ("b", ctypes.c_uint64), ("c", ctypes.c_uint64),
                    ("d", vp), ("e", vp), ("internal", vp)]

    rp = ctypes.POINTER(Result)
    lib.duckdb_open.argtypes = [ctypes.c_char_p, ctypes.POINTER(vp)]
    lib.duckdb_connect.argtypes = [vp, ctypes.POINTER(vp)]
    lib.duckdb_query.argtypes = [vp, ctypes.c_char_p, rp]
    lib.duckdb_result_error.argtypes = [rp]
    lib.duckdb_result_error.restype = ctypes.c_char_p
    lib.duckdb_destroy_result.argtypes = [rp]
    lib.duckdb_column_count.argtypes = [rp]
    lib.duckdb_column_count.restype = ctypes.c_uint64
    lib.duckdb_row_count.argtypes = [rp]
    lib.duckdb_row_count.restype = ctypes.c_uint64
    lib.duckdb_value_varchar.argtypes = [rp, ctypes.c_uint64, ctypes.c_uint64]
    lib.duckdb_value_varchar.restype = vp
    lib.duckdb_value_is_null.argtypes = [rp, ctypes.c_uint64, ctypes.c_uint64]
    lib.duckdb_value_is_null.restype = ctypes.c_bool
    lib.duckdb_free.argtypes = [vp]
    db, con = vp(), vp()
    if lib.duckdb_open(spec["db"].encode(), ctypes.byref(db)) != 0:
        return {"error": "open failed", "rss": 0}
    lib.duckdb_connect(db, ctypes.byref(con))

    def query(sql, fetch=False):
        res = Result()
        state = lib.duckdb_query(con, sql.encode(), ctypes.byref(res))
        if state != 0:
            err = (lib.duckdb_result_error(ctypes.byref(res)) or b"?").decode()
            lib.duckdb_destroy_result(ctypes.byref(res))
            raise RuntimeError(err)
        text = None
        if fetch:
            rows = []
            cols = lib.duckdb_column_count(ctypes.byref(res))
            for r in range(min(lib.duckdb_row_count(ctypes.byref(res)), 100000)):
                cells = []
                for c in range(cols):
                    if lib.duckdb_value_is_null(ctypes.byref(res), c, r):
                        cells.append("NULL")
                    else:
                        ptr = lib.duckdb_value_varchar(ctypes.byref(res), c, r)
                        cells.append(ctypes.string_at(ptr).decode(errors="replace"))
                        lib.duckdb_free(ptr)
                rows.append(",".join(cells))
            text = "\n".join(rows)
        lib.duckdb_destroy_result(ctypes.byref(res))
        return text

    def cpu():
        u = resource.getrusage(resource.RUSAGE_SELF)
        return u.ru_utime + u.ru_stime

    def rss():
        return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss

    try:
        query(f"SET threads={spec['threads']}")
        if spec.get("memory_limit"):
            query(f"SET memory_limit='{spec['memory_limit']}'")
        check = spec.get("check")
        result = query(spec["sql"], fetch=not check)
        if check:
            result = query(check, fetch=True)
            query("DROP TABLE ans")
        times = []
        for _ in range(spec["reps"]):
            w0, c0 = time.perf_counter(), cpu()
            query(spec["sql"])
            times.append((time.perf_counter() - w0, cpu() - c0))
            if check:
                query("DROP TABLE ans")
    except RuntimeError as e:
        return {"error": str(e)[:300], "rss": rss()}
    best = min(times)
    return {"wall": best[0], "cpu": best[1], "rss": rss(), "result": result, "error": None}


def run(engine, db, sql, check, threads, reps, memory_limit):
    spec = {"db": db, "sql": sql, "check": check, "threads": threads, "reps": reps, "memory_limit": memory_limit}
    # A watchdog: a hung engine fails the query instead of stalling the run.
    limit = float(os.environ.get("BENCH_TIMEOUT", "600"))
    try:
        proc = subprocess.run([sys.executable, "-I", os.path.abspath(__file__), "--child", engine],
                              input=json.dumps(spec), capture_output=True, text=True, timeout=limit)
    except subprocess.TimeoutExpired:
        return {"error": f"timed out after {limit:.0f} s", "rss": 0}
    try:
        return json.loads(proc.stdout.strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError):
        return {"error": f"crashed ({proc.returncode}): {proc.stderr.strip()[-300:]}", "rss": 0}


def same(a, b):
    if a == b:
        return True
    la, lb = a.splitlines(), b.splitlines()
    if len(la) != len(lb):
        return False
    for x, y in zip(la, lb):
        fx, fy = x.split(","), y.split(",")
        if len(fx) != len(fy):
            return False
        for u, v in zip(fx, fy):
            if u == v:
                continue
            try:
                fu, fv = float(u), float(v)
            except ValueError:
                return False
            if not math.isclose(fu, fv, rel_tol=1e-9, abs_tol=1e-9):
                return False
    return True


def tie_check(db, sql):
    """For `ORDER BY ... LIMIT` queries whose ties make the result order-dependent: the
    whole result without ORDER BY / LIMIT must hash the same on both engines (1 thread)."""
    base = re.sub(r"(?is)\s+LIMIT\s+\d+(\s+OFFSET\s+\d+)?$", "", sql)
    base = re.sub(r"(?is)\s+ORDER\s+BY\s+[^()]*$", "", base)
    if base == sql:
        return False
    q = f"SET threads=1; SELECT count(*), sum(hash(t)::HUGEINT) FROM ({base}) t"
    outs = [subprocess.run([CLIS[e], db, "-csv", "-c", q], capture_output=True, text=True, timeout=600).stdout
            for e in ("duck", "rust")]
    return outs[0] == outs[1] and outs[0].count("\n") == 2


def geomean(xs):
    xs = [x for x in xs if x and x > 0]
    return math.exp(sum(math.log(x) for x in xs) / len(xs)) if xs else float("nan")


def interpreter_rss():
    """Peak RSS of the measuring process before it loads an engine (the Python
    interpreter and ctypes), subtracted from both engines' peaks."""
    code = "import ctypes, json, resource; print(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)"
    out = subprocess.run([sys.executable, "-I", "-c", code], capture_output=True, text=True).stdout
    return int(out.strip() or 0)


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--child":
        print(json.dumps(child(sys.argv[2], json.loads(sys.stdin.read()))))
        return
    p = argparse.ArgumentParser()
    p.add_argument("suite")
    p.add_argument("--threads", type=int, default=14)
    p.add_argument("--reps", type=int, default=5)
    p.add_argument("--queries", default=None)
    p.add_argument("--engines", default="duck,rust")
    p.add_argument("--memory-limit", default=None)
    p.add_argument("--out", default=None)
    p.add_argument("--no-check", action="store_true")
    p.add_argument("--dump", default=None, help="write each engine's checked result here")
    p.add_argument("--sql", action="append", default=[], help="queries of an ad-hoc suite (SUITE = a .duckdb file)")
    a = p.parse_args()
    ADHOC.extend(a.sql)
    engines = a.engines.split(",")
    queries = suite_queries(a.suite)
    if a.queries:
        wanted = {q if q.startswith("q") else f"q{int(q):02d}" for q in a.queries.split(",")}
        queries = [q for q in queries if q[0] in wanted]
    if not engines or len(set(engines)) != len(engines) or any(e not in LIBS for e in engines):
        p.error("--engines must select duck, rust, or duck,rust without duplicates")
    if not queries:
        p.error("no queries selected")
    both = set(engines) == {"duck", "rust"}
    base = interpreter_rss()
    print(f"{a.suite} threads={a.threads} reps={a.reps} (mem x: peak RSS above the interpreter's "
          f"{base / 1e6:.0f} MB)")
    hdr = "q    " + "".join(f"| {e} wall     cpu   rssMB " for e in engines)
    if both:
        hdr += "| speed | cpu x | mem x | check"
    print(hdr, flush=True)
    rows = {}
    failed = []
    for name, db, sql, check in queries:
        res = {e: run(e, db, sql, check, a.threads, a.reps, a.memory_limit) for e in engines}
        rows[name] = res
        line = f"{name:5}"
        for e in engines:
            r = res[e]
            if "wall" in r:
                line += f"| {r['wall']:9.4f} {r['cpu']:6.3f} {r['rss'] / 1e6:6.0f} "
            else:
                line += f"| ERR {r['error'][:60]} "
        if any("wall" not in res[e] for e in engines):
            res["check"] = "error"
            failed.append(name)
        if both and all("wall" in res[e] for e in engines):
            d, r = res["duck"], res["rust"]
            sp = d["wall"] / r["wall"]
            cp = d["cpu"] / max(r["cpu"], 1e-6)
            mem = max(r["rss"] - base, 1) / max(d["rss"] - base, 1)
            line += f"| {sp:5.2f} | {cp:5.2f} | {mem:5.2f} "
            res["check"] = "unchecked"
            if not a.no_check:
                if same(d["result"], r["result"]):
                    res["check"] = "ok"
                elif same("\n".join(sorted(d["result"].splitlines())), "\n".join(sorted(r["result"].splitlines()))):
                    res["check"] = "ok (order)"
                elif tie_check(db, sql):
                    res["check"] = "ok (ties)"
                else:
                    res["check"] = "DIFF"
                    failed.append(name)
                line += "| " + res["check"]
                if a.dump:
                    os.makedirs(a.dump, exist_ok=True)
                    for e in engines:
                        open(os.path.join(a.dump, f"{a.suite}-{name}-{e}.txt"), "w").write(res[e]["result"])
            if res["check"] != "DIFF":
                res["speed"], res["mem"], res["mem_raw"] = sp, mem, r["rss"] / d["rss"]
        print(line, flush=True)
    if both:
        ok = [r for r in rows.values() if "speed" in r]
        if ok:
            tot = {e: sum(r[e]["wall"] for r in ok) for e in engines}
            print(f"total wall duck {tot['duck']:.3f} rust {tot['rust']:.3f}; geomean speed "
                  f"{geomean([r['speed'] for r in ok]):.3f} (min {min(r['speed'] for r in ok):.2f}); geomean mem "
                  f"{geomean([r['mem'] for r in ok]):.3f} (max {max(r['mem'] for r in ok):.2f}); peak rss duck "
                  f"{max(r['duck']['rss'] for r in ok) / 1e6:.0f} MB rust {max(r['rust']['rss'] for r in ok) / 1e6:.0f} MB")
    if a.out:
        for r in rows.values():
            for e in engines:
                r[e].pop("result", None)
        with open(a.out, "w") as output:
            json.dump({"suite": a.suite, "threads": a.threads, "interpreter_rss": base, "rows": rows},
                      output, indent=1)
    if failed:
        sys.exit("benchmark validation failed: " + ", ".join(failed))


if __name__ == "__main__":
    main()
