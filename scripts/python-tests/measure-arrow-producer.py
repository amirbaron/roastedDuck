"""Result-checked Arrow producer scans with coupled elapsed/CPU/RSS."""
import gc,json,pathlib,resource,sys,time
import duckdb,numpy as np,pyarrow as pa,pyarrow.dataset as ds,pyarrow.parquet as pq
mode,rows,fixture,output=sys.argv[1],int(sys.argv[2]),pathlib.Path(sys.argv[3]),pathlib.Path(sys.argv[4]);c=duckdb.connect(config={'threads':1});c.sql('select 42').fetchall()
if mode.startswith('table'):
 source=pa.table({f'v{i}':np.arange(rows,dtype='int64')for i in range(8)})
else:source=ds.dataset(fixture,format='parquet')
condition='v0<1024'if mode.endswith('filter')else'v0>=0';expected=1024*1023//2 if mode.endswith('filter')else rows*(rows-1)//2;count=1024 if mode.endswith('filter')else rows
# Warm only a small separate producer; retain the measured source in baseline RSS.
c.from_arrow(pa.table({'i':[1]})).fetchall();gc.collect();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter();result=c.sql(f'select count(*),sum(v0)from source where {condition}').fetchone();elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF);assert result==(count,expected),(result,count,expected)
report={'mode':mode,'rows':rows,'columns':8,'output_count':result[0],'checksum':result[1],'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'conditions':'fresh macOS process;oneengine thread;source construction excludedtime/includedbaselineRSS;simple small producer warmed;Rustdevelopment vsCpprelease;notfinalacceptance'};output.write_text(json.dumps(report,indent=2)+'\n');c.close()
