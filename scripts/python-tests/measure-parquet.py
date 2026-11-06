"""Fresh native Parquet read: factory+query wall/CPU time, correctness and RSS."""
import gc,io,json,resource,sys,time
from pathlib import Path
import duckdb,fsspec
import pyarrow as pa
import pyarrow.parquet as pq
mode,filename,rows,output=sys.argv[1],sys.argv[2],int(sys.argv[3]),sys.argv[4]
con=duckdb.connect(config={'threads':1});small=io.BytesIO();pq.write_table(pa.table({'a':[1],'b':[2]}),small)
con.read_parquet(io.BytesIO(small.getvalue())).aggregate('sum(a),sum(b)').fetchall()
source=filename if mode=='path' else io.BytesIO(Path(filename).read_bytes())
gc.collect();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter()
result=con.read_parquet(source).aggregate('sum(a),sum(b)').fetchone()
elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF)
expected=rows*(rows-1)//2
assert result==(expected,expected*2),result
report={'mode':mode,'rows':rows,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'result':list(result),'conditions':'fresh macOS process, one engine thread, warmed Parquet/imports; source prepared before measurement; Rust development vs C++ release; not final acceptance'}
Path(output).write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
