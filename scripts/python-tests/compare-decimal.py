"""Isolated real Decimal binding, Pandas and storage/cast outcomes, without normalization."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import resource
import struct
import subprocess
import sys
import time


VALUES = ["0", "-0", "0.00", "-0.00", "1", "-12.34", "0.001", "-0.00001",
          "1.2300", "1E1", "-1E1", "1E5", "123E5", "0E5", "1E18", "1E19",
          "1E20", "1E21", "-1E21", "1E37", "1E38", "1E39", "1E255", "1E256",
          "1E257", "1E-18", "1E-37", "1E-38", "1E-39", "1E-255", "1E-256",
          "1E-257", "99999999999999999999999999999999999999",
          "999999999999999999999999999999999999999", "NaN", "-NaN", "sNaN", "-sNaN",
          "Infinity", "-Infinity", "1E2147483647", "1E2147483648", "1E-2147483648",
          "1E-2147483649"]


def worker(index, output, nested=False):
    from decimal import Decimal
    import duckdb
    import pandas as pd

    rss_scale = 1 if sys.platform == "darwin" else 1024
    baseline_rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * rss_scale
    value = Decimal(VALUES[index])

    def encoded(x):
        if isinstance(x, Decimal):
            t = x.as_tuple()
            return {"decimal": str(x), "sign": t.sign, "digits": list(t.digits), "exponent": t.exponent}
        if isinstance(x, float):
            return {"float_hex": x.hex(), "float_bits": struct.pack("!d", x).hex()}
        if isinstance(x, (list, tuple)):
            return [encoded(y) for y in x]
        if isinstance(x, dict):
            return {key: encoded(value) for key, value in x.items()}
        return x

    outcomes = {}
    timings = {}
    routes = (["pandas-nested-list", "pandas-nested-struct"] if nested else
              ["parameter", "boxed", "insert-double", "cast-decimal"])
    # These are binder/caster limit probes, not unbounded Pandas type-inference probes.
    if not nested and index < 40:
        routes += ["pandas-single", "pandas-mixed"]
    for route in routes:
        started = time.perf_counter()
        con = duckdb.connect()
        try:
            if route == "parameter":
                result = con.execute("SELECT typeof(?), ?::VARCHAR, ?", [value] * 3)
            elif route == "boxed":
                boxed = duckdb.Value(value, duckdb.sqltypes.DOUBLE)
                result = con.execute("SELECT typeof(?), ?::VARCHAR, ?", [boxed] * 3)
            elif route == "insert-double":
                con.execute("CREATE TABLE inserted(v DOUBLE)")
                con.execute("INSERT INTO inserted VALUES (?)", [value])
                result = con.execute("SELECT typeof(v), v::VARCHAR, v FROM inserted")
            elif route == "cast-decimal":
                result = con.execute("SELECT CAST(? AS DECIMAL(38,10))", [value])
            else:
                if route == "pandas-nested-list":
                    values = [[value, Decimal("1.23"), None]]
                elif route == "pandas-nested-struct":
                    values = [{"target": value, "base": Decimal("1.23")}]
                else:
                    values = [value] if route == "pandas-single" else [value, Decimal("1.23"), None]
                frame = pd.DataFrame({"v": pd.Series(values, dtype=object)})
                con.register("decimal_frame", frame)
                result = con.execute("SELECT typeof(v), v::VARCHAR, v FROM decimal_frame")
            outcomes[route] = {"description": [str(x[1]) for x in result.description],
                               "rows": encoded(result.fetchall())}
        except Exception as error:
            outcomes[route] = {"error": type(error).__name__, "message": str(error)}
        finally:
            con.close()
        timings[route] = time.perf_counter() - started
        peak_rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * rss_scale
        output.write_text(json.dumps({"value": VALUES[index], "outcomes": outcomes,
                                      "performance": {"route_elapsed_seconds": timings,
                                                      "baseline_rss_bytes": baseline_rss,
                                                      "peak_rss_bytes": peak_rss,
                                                      "peak_above_imports_bytes": peak_rss - baseline_rss}}, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", type=Path)
    parser.add_argument("--work", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--worker", type=int)
    parser.add_argument("--worker-out", type=Path)
    parser.add_argument("--nested", action="store_true")
    args = parser.parse_args()
    if args.worker is not None:
        worker(args.worker, args.worker_out, args.nested)
        return
    if not all([args.python, args.work, args.out]):
        parser.error("--python --work --out are required")
    if args.work.exists() or args.out.exists():
        parser.error("use fresh work/output paths to preserve evidence")
    root = args.work.resolve()
    root.mkdir(parents=True)
    script = Path(__file__).resolve()
    rows = []
    with args.out.open("x") as evidence:
        for index, value in enumerate(VALUES[:40] if args.nested else VALUES):
            directory = root / f"case-{index:03d}"
            directory.mkdir()
            (directory / "tmp").mkdir()
            output = directory / "outcomes.json"
            env = dict(os.environ, TMPDIR=str(directory / "tmp"),
                       PYTHONPYCACHEPREFIX=str(directory / "pycache"))
            try:
                command = ["nice", "-n", "10", str(args.python.absolute()), str(script),
                           "--worker", str(index), "--worker-out", str(output)]
                if args.nested:
                    command.append("--nested")
                run = subprocess.run(command,
                                     cwd=directory, env=env, capture_output=True, timeout=20)
                status = {"exit": run.returncode, "stdout_base64": base64.b64encode(run.stdout).decode(),
                          "stderr_base64": base64.b64encode(run.stderr).decode()}
            except subprocess.TimeoutExpired as error:
                status = {"timeout": True, "stdout_base64": base64.b64encode(error.stdout or b"").decode(),
                          "stderr_base64": base64.b64encode(error.stderr or b"").decode()}
            details = json.loads(output.read_text()) if output.exists() else {}
            row = {"value": value, **status, "outcomes": details.get("outcomes", {}),
                   "performance": details.get("performance")}
            evidence.write(json.dumps(row) + "\n")
            evidence.flush()
            rows.append(row)
            if index == 0 and status.get("exit") != 0:
                break
    summary = {"values": len(rows), "nested": args.nested, "outcomes": sum(len(x["outcomes"]) for x in rows),
               "abnormal": [x["value"] for x in rows if x.get("exit") != 0],
               "python": str(args.python.absolute()), "source_sha256": hashlib.sha256(script.read_bytes()).hexdigest(),
               "normalization": "none; explicit Decimal tuple and float-bit JSON representations"}
    (root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary))
    raise SystemExit(bool(summary["abnormal"]))


if __name__ == "__main__":
    main()
