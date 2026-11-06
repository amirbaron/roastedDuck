"""Checked connection query/alias paths with paired time and whole-process RSS."""
import gc
import json
import resource
import sys
import time

import duckdb

method = sys.argv[1]
assert method in ['sql', 'query', 'from_query', 'set_alias']
con = duckdb.connect(config={'threads': 1})
base = con.sql('SELECT 42')
if method == 'set_alias':
    def operation():
        return base.set_alias('bench').fetchall()[0][0]
else:
    function = getattr(con, method)
    def operation():
        return function('SELECT 42', alias='bench').fetchall()[0][0]
assert operation() == 42
gc.collect()
before = resource.getrusage(resource.RUSAGE_SELF)
start = time.perf_counter()
checksum = sum(operation() for _ in range(1000))
elapsed = time.perf_counter() - start
after = resource.getrusage(resource.RUSAGE_SELF)
assert checksum == 42000
con.close()
print(json.dumps({'method': method, 'iterations': 1000, 'elapsed_seconds': elapsed,
                  'cpu_seconds': after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime,
                  'peak_rss_bytes': after.ru_maxrss, 'baseline_peak_rss_bytes': before.ru_maxrss,
                  'checksum': checksum, 'conditions': 'fresh macOS process, warmed query, one engine thread; not final release acceptance'}))
