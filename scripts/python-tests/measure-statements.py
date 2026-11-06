"""Result-checked execution paths with coupled elapsed/CPU/whole-process RSS."""
import gc,json,pathlib,resource,sys,time
import duckdb
mode,rows,output=sys.argv[1],int(sys.argv[2]),pathlib.Path(sys.argv[3]);c=duckdb.connect(config={'threads':1});c.execute('select 42').fetchall()
if mode=='many':
 c.execute('create table t(i bigint)');parameters=[(i,)for i in range(rows)];statement=c.extract_statements('insert into t values($1)')[0]
else:statement=c.extract_statements('select $1+1')[0]
gc.collect();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter()
if mode=='many':c.executemany(statement,parameters);assert c.fetchall()==[(1,)]
else:
 checksum=0
 for i in range(rows):
  query=statement if mode=='parsed'else'select $1+1';checksum+=c.execute(query,[i]).fetchone()[0]
elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF)
if mode=='many':count,checksum=c.execute('select count(*),sum(i)from t').fetchone();assert (count,checksum)==(rows,rows*(rows-1)//2)
else:assert checksum==rows*(rows+1)//2
result={'mode':mode,'rows':rows,'checksum':checksum,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'conditions':'fresh macOS process;1engine thread;control query warmed;parameter construction excluded from time but in baselineRSS;Rustdevelopment vsCpprelease;not final acceptance'};output.write_text(json.dumps(result,indent=2)+'\n');c.close()
