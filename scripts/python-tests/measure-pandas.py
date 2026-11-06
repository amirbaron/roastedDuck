"""One query per process: correctness, scan wall time and whole-process peak RSS."""
import gc
import json
from pathlib import Path
import resource
import sys
import time
import duckdb
import numpy as np
import pandas as pd

rows = int(sys.argv[1])
data = np.arange(rows, dtype='int64')
frame = pd.DataFrame({'i': data, 'd': data.astype('float64') * .5, 'n': pd.array(data, dtype='Int64')})
frame.loc[frame.index % 5 == 0, 'n'] = pd.NA
with duckdb.connect(config={'threads': 1}) as con:
    con.from_df(frame.head(10)).aggregate('sum(i)').fetchall()
    gc.collect()
    before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    start = time.perf_counter()
    result = con.from_df(frame).aggregate('sum(i), sum(d), count(n)').fetchone()
    elapsed = time.perf_counter() - start
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
expected_sum = rows * (rows - 1) // 2
assert result == (expected_sum, expected_sum * .5, rows - ((rows + 4) // 5)), result
report = {'version': duckdb.__version__, 'rows': rows, 'elapsed_seconds': elapsed,
          'peak_rss_bytes': peak, 'baseline_peak_rss_bytes': before,
          'incremental_peak_rss_bytes': peak - before, 'result': list(result),
          'conditions': 'one fresh macOS process, one engine thread, input and imports warmed; Rust development vs C++ release; not final performance acceptance'}
Path(sys.argv[2]).write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report))
