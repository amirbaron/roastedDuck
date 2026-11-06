"""Bounded first-batch measurement; individual process wall time and peak RSS, no suite average."""
import gc
import json
from pathlib import Path
import resource
import sys
import time
import duckdb
import pyarrow

rows = int(sys.argv[1])
with duckdb.connect(config={'threads': 1}) as connection:
    # Load the host reader machinery before taking the process baseline.
    warmup = connection.sql('SELECT 1 AS i').to_arrow_reader(1024)
    warmup.read_all()
    warmup.close()
    del warmup
    gc.collect()
    before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    start = time.perf_counter()
    reader = connection.sql(f'SELECT i FROM range({rows}) t(i)').to_arrow_reader(1024)
    first = reader.read_next_batch()
    elapsed = time.perf_counter() - start
    assert first.num_rows == 1024
    assert first.column(0)[0].as_py() == 0
    assert first.column(0)[1023].as_py() == 1023
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    reader.close()
    recovered = connection.execute('SELECT 42').fetchone()
    assert recovered == (42,)
report = {'version': duckdb.__version__, 'query_rows': rows, 'consumed_rows': 1024,
          'elapsed_seconds': elapsed, 'peak_rss_bytes': peak, 'baseline_peak_rss_bytes': before,
          'incremental_peak_rss_bytes': peak - before, 'rss_units': 'bytes on macOS',
          'measurement': 'single fresh process; imports warmed; first batch only; not full-query performance acceptance',
          'recovery': list(recovered)}
Path(sys.argv[2]).write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report))
