"""Checked JSON scan or prefix, with coupled elapsed/CPU and whole-process peak memory."""
import gc,json,os,resource,sys,time
from io import BytesIO
from pathlib import Path
import duckdb
mode,source,threads,output=sys.argv[1],Path(sys.argv[2]),int(sys.argv[3]),Path(sys.argv[4])
rows=int(source.stem.split('-')[-1]);con=duckdb.connect(config={'threads':threads})
input=BytesIO(source.read_bytes())if 'memory'in mode else str(source)
con.sql('select 42').fetchall();gc.collect();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter()
rel=con.read_json(input)
if 'prefix'in mode:result=rel.limit(3).fetchall();expected=[(0,),(1,),(2,)]
else:result=rel.aggregate('count(*) n,sum(i) s').fetchall();expected=[(rows,rows*(rows-1)//2)]
elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF);assert result==expected,(result,expected)
report={'mode':mode,'rows':rows,'threads':threads,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'result':result,'source_bytes':source.stat().st_size,'conditions':'fresh macOS process; input BytesIO prepared before baseline; relation construction/native schema detection plus query; Rust development vs C++ release; not final acceptance'}
output.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
