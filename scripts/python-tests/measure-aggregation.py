"""Fresh-process native grouping/window query, correctness and coupled speed/RSS."""
import gc,json,resource,sys,time
from pathlib import Path
import duckdb
mode,rows,output=sys.argv[1],int(sys.argv[2]),sys.argv[3]
con=duckdb.connect(config={'threads':1});con.sql('select i,i%100 g from range(1000)t(i)').sum('i',groups='g',projected_columns='g').fetchall()
gc.collect();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter()
rel=con.sql(f'select i,i%100 g from range({rows})t(i)')
if mode in ['group','sql-group']:
 result=(con.sql(f'select g,sum(i) from(select i,i%100 g from range({rows})t(i))t group by g order by g') if mode=='sql-group' else rel.sum('i',groups='g',projected_columns='g').order('g')).fetchall();assert len(result)==100;checksum=sum(r[1]for r in result);expected=rows*(rows-1)//2
else:
 result=(con.sql(f'select sum(rn) from(select row_number() over(partition by g order by i) rn from(select i,i%100 g from range({rows})t(i))t)t') if mode=='sql-window' else rel.row_number('over(partition by g order by i)',projected_columns='i,g').aggregate('sum(#3)')).fetchall();checksum=result[0][0];q,r=divmod(rows,100);expected=(100-r)*q*(q+1)//2+r*(q+1)*(q+2)//2
elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF);assert checksum==expected,(checksum,expected)
report={'mode':mode,'rows':rows,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'checksum':checksum,'conditions':'fresh macOS process, one engine thread, imports/basic grouping warmed; Rust development vs C++ release; not final acceptance'}
Path(output).write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
