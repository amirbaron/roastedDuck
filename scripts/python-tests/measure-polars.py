"""Native Polars eager/lazy journeys with verified results and coupled elapsed/CPU/RSS."""
import gc,json,resource,sys,time
from pathlib import Path
import duckdb
import polars as pl
mode,rows,output=sys.argv[1],int(sys.argv[2]),Path(sys.argv[3])
c=duckdb.connect(config={'threads':1});c.sql('select 42 i').pl();gc.collect();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter()
rel=c.execute(f'select i from range({rows})t(i)')if mode=='connection-lazy'else c.sql(f'select i from range({rows})t(i)')
if mode=='eager':result=rel.pl();count=result.height;checksum=result['i'].sum();expected=rows*(rows-1)//2
elif mode=='connection-lazy':result=rel.pl(lazy=True).collect();count=result.height;checksum=result['i'].sum();expected=rows*(rows-1)//2
elif mode=='lazy-filter':result=rel.pl(lazy=True).filter(pl.col('i')>=rows//2).select(pl.col('i').sum()).collect();count=rows-rows//2;checksum=result['i'][0];expected=rows*(rows-1)//2-(rows//2)*(rows//2-1)//2
else:result=rel.pl(lazy=True).head(1024).collect();count=result.height;checksum=result['i'].sum();expected=1024*1023//2
elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF);assert checksum==expected,(checksum,expected)
report={'mode':mode,'rows':rows,'output_rows':count,'checksum':checksum,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'conditions':'fresh macOS process, one engine thread, basic Polars conversion warmed; native Arrow conversion and Polars query; Rust development vs C++ release; not final acceptance'}
output.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
