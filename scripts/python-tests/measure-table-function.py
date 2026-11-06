"""Checked native table-function relation construction with timing and RSS."""
import gc
import json
import resource
import time
import duckdb

con = duckdb.connect(config={'threads': 1})
assert con.table_function('range', [2]).fetchall() == [(0,), (1,)]
gc.collect()
before = resource.getrusage(resource.RUSAGE_SELF)
start = time.perf_counter()
count = 0
for _ in range(1000):
    rows = con.table_function('range', [2]).fetchall()
    assert rows == [(0,), (1,)]
    count += len(rows)
elapsed = time.perf_counter() - start
after = resource.getrusage(resource.RUSAGE_SELF)
con.close()
print(json.dumps({'iterations': 1000, 'rows': count, 'elapsed_seconds': elapsed,
                  'peak_rss_bytes': after.ru_maxrss, 'baseline_peak_rss_bytes': before.ru_maxrss,
                  'cpu_seconds': after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime,
                  'conditions': 'fresh macOS process,oneengine thread,warmed,resultchecked;notfinalacceptance'}))
