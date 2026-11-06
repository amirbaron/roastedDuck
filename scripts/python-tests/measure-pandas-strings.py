"""Coupled time/RSS measurement for a real nullable extension-string scan."""
import gc
import json
import os
from pathlib import Path
import resource
import sys
import time

import duckdb
import pandas as pd

rows = int(sys.argv[1])
storage = sys.argv[2]
rss_multiplier = 1 if sys.platform == "darwin" else 1024
assert rows % 4 == 0
frame = pd.DataFrame({"x": pd.Series(["duck", "雪", None, "quack"] * (rows // 4), dtype=f"string[{storage}]")})
with duckdb.connect(config={"threads": 1}) as con:
    con.from_df(frame.head(8)).aggregate("count(x), sum(length(x))").fetchall()
    gc.collect()
    before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * rss_multiplier
    start = time.perf_counter()
    result = con.from_df(frame).aggregate("count(x), sum(length(x))").fetchone()
    elapsed = time.perf_counter() - start
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * rss_multiplier
assert result == (rows * 3 // 4, rows * 10 // 4), result
report = {
    "rows": rows, "storage": storage, "elapsed_seconds": elapsed,
    "peak_rss_bytes": peak, "baseline_peak_rss_bytes": before,
    "incremental_peak_rss_bytes": peak - before, "result": result,
    "conditions": f"fresh {sys.platform} process, one engine thread, warmed imports/input; scoped measurement, not final acceptance",
    "build_provenance": os.environ.get("RUSTDB_MEASUREMENT_BUILD_LABEL", "unspecified"),
}
Path(sys.argv[3]).write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report))
