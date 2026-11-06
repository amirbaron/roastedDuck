"""Fresh process, one scalar/Arrow UDF query, bounded callback batches and RSS."""
import gc
import json
from pathlib import Path
import resource
import sys
import time
import duckdb
import numpy as np
import pandas as pd
import pyarrow as pa
import pyarrow.compute as pc

mode,rows,output=sys.argv[1],int(sys.argv[2]),sys.argv[3]
con=duckdb.connect(config={'threads':1})
def native(x):return x+1
def arrow(x):return pc.add(x,1)
con.create_function('f',arrow if mode=='arrow' else native,[int],int,type=mode,side_effects=True)
con.sql('select sum(f(i)) from range(10)t(i)').fetchall()
gc.collect()
before=resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
start=time.perf_counter()
result=con.sql(f'select sum(f(i)) from range({rows})t(i)').fetchone()
elapsed=time.perf_counter()-start
peak=resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
assert result==(rows*(rows+1)//2,),result
report={'version':duckdb.__version__,'mode':mode,'rows':rows,'elapsed_seconds':elapsed,
 'peak_rss_bytes':peak,'baseline_peak_rss_bytes':before,'incremental_peak_rss_bytes':peak-before,'result':list(result),
 'conditions':'one fresh macOS process, one engine thread, initialized imports/function; Rust development vs C++ release; scoped microbenchmark only'}
Path(output).write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report))
