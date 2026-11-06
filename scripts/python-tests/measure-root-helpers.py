"""Checked default/explicit module helper overhead, paired with whole-process RSS."""
import argparse
import gc
import json
import resource
import time

import duckdb

parser = argparse.ArgumentParser()
parser.add_argument('mode', choices=['explicit', 'default'])
parser.add_argument('--iterations', type=int, default=1000)
args = parser.parse_args()
con = duckdb.connect(config={'threads': 1})
duckdb.set_default_connection(con)
kwargs = {'connection': con} if args.mode == 'explicit' else {}
assert duckdb.execute('SELECT 42', **kwargs).fetchone() == (42,)
gc.collect()
before = resource.getrusage(resource.RUSAGE_SELF)
start = time.perf_counter()
checksum = 0
for i in range(args.iterations):
    checksum += duckdb.execute('SELECT $1 + 1', [i], **kwargs).fetchone()[0]
elapsed = time.perf_counter() - start
after = resource.getrusage(resource.RUSAGE_SELF)
assert checksum == args.iterations * (args.iterations + 1) // 2
con.close()
print(json.dumps({'mode': args.mode, 'iterations': args.iterations,
                  'elapsed_seconds': elapsed, 'peak_rss_bytes': after.ru_maxrss,
                  'baseline_peak_rss_bytes': before.ru_maxrss,
                  'cpu_seconds': after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime,
                  'checksum': checksum, 'rss_units': 'bytes on macOS',
                  'conditions': 'fresh process, one engine thread, warmed, actual module helper calls; not final benchmark acceptance'}))
