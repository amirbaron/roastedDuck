"""A fresh one-thread CSV query: checksum, factory+execution time and peak memory."""
import gc,io,json,resource,sys,time
from pathlib import Path
import duckdb
import fsspec
mode,filename,rows,output=sys.argv[1],sys.argv[2],int(sys.argv[3]),sys.argv[4]
con=duckdb.connect(config={'threads':1})
con.read_csv(io.BytesIO(b'a,b\n1,2\n')).aggregate('sum(a),sum(b)').fetchall()
source=filename if mode=='path' else io.BytesIO(Path(filename).read_bytes())
gc.collect();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter()
result=con.read_csv(source).aggregate('sum(a),sum(b)').fetchone()
elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF)
expected=rows*(rows-1)//2
assert result==(expected,expected*2),result
report={'mode':mode,'rows':rows,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'result':list(result),'conditions':'fresh macOS process, one engine thread, warmed CSV imports; source prepared before measurement; Rust development vs C++ release; not release acceptance'}
Path(output).write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
