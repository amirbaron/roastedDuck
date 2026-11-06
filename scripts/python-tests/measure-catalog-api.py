"""Checked metadata/registration timing paired with whole-process RSS."""
import gc
import json
import resource
import sys
import time

import duckdb
import pandas as pd

mode = sys.argv[1]
assert mode in ['physical', 'registered_table', 'registered_view', 'register']
con = duckdb.connect(config={'threads': 1})
con.execute('CREATE TABLE physical AS SELECT 42 i')
frame = pd.DataFrame({'i': [42]})
con.register('registered', frame)

def operation():
    if mode == 'register':
        con.register('registered', frame)
        assert con.view('registered').columns == ['i']
    else:
        name = 'physical' if mode == 'physical' else 'registered'
        method = con.view if mode == 'registered_view' else con.table
        assert method(name).columns == ['i']

operation()
gc.collect()
before = resource.getrusage(resource.RUSAGE_SELF)
start = time.perf_counter()
for _ in range(1000):
    operation()
elapsed = time.perf_counter() - start
after = resource.getrusage(resource.RUSAGE_SELF)
assert con.view('registered').fetchall() == [(42,)]
con.unregister('registered')
con.close()
print(json.dumps({'mode': mode, 'iterations': 1000, 'elapsed_seconds': elapsed,
                  'cpu_seconds': after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime,
                  'peak_rss_bytes': after.ru_maxrss, 'baseline_peak_rss_bytes': before.ru_maxrss,
                  'conditions': 'fresh macOS process, one engine thread, warmed; metadata/result checks; not final release acceptance'}))
