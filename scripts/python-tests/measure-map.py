"""One native dataframe-map query with result checks and coupled speed/peak memory."""
import gc,json,resource,sys,time
from pathlib import Path
import duckdb
import numpy as np
import pandas as pd
mode,rows,output=sys.argv[1],int(sys.argv[2]),sys.argv[3]
con=duckdb.connect(config={'threads':1});latest=[None];seen=[0,0]
def mapper(df):
 seen[0]+=len(df);seen[1]=max(seen[1],len(df))
 # Frozen C++ aliases numeric output buffers beyond callback return. Keep only the
 # latest output frame alive for both engines; never retain the whole input/query.
 latest[0]=df if mode in ['identity','prefix'] else df.assign(i=df.i+1)
 return latest[0]
con.sql('select i from range(10)t(i)').map(mapper,schema={'i':int}).aggregate('sum(i)').fetchall()
gc.collect();seen[:]=[0,0];before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter()
rel=con.sql(f'select i from range({rows})t(i)').map(mapper,schema={'i':int})
if mode=='prefix':rel=rel.limit(3)
result=rel.aggregate('count(*) n,sum(i) s').fetchone()
elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF)
expected=3 if mode=='prefix'else rows*(rows-1)//2+(rows if mode!='identity'else 0)
assert result==(3 if mode=='prefix'else rows,expected),(result,expected)
assert (seen[0]<=2048 if mode=='prefix'else seen[0]==rows) and seen[1]<=2048,seen
report={'mode':mode,'rows':rows,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'result':list(result),'processed_rows':seen[0],'largest_batch':seen[1],'conditions':'fresh macOS process, one engine thread, imports/map warmed; retain only latest output dataframe on BOTH engines to avoid frozen C++ numeric-buffer lifetime bug; Rust development vs C++ release; not final acceptance'}
Path(output).write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
