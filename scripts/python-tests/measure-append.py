"""Result-checked dataframe append with coupled native time and process RSS."""
import gc,json,pathlib,resource,sys,time
import duckdb,numpy as np,pandas as pd
rows=int(sys.argv[1]);by_name=sys.argv[2]=='by-name';output=pathlib.Path(sys.argv[3]);con=duckdb.connect(config={'threads':1})
con.execute('create table warm(i bigint)');con.append('warm',pd.DataFrame({'i':np.arange(16,dtype='int64')}));con.execute('drop table warm')
con.execute('create table appended(i bigint)');frame=pd.DataFrame({'i':np.arange(rows,dtype='int64')});gc.collect();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter();con.append('appended',frame,by_name=by_name);elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF)
observed=con.execute('select count(*),sum(i) from appended').fetchone();assert observed==(rows,rows*(rows-1)//2),(observed,rows)
report={'rows':rows,'by_name':by_name,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,'incremental_peak_rss_bytes':after.ru_maxrss-before.ru_maxrss,'count':observed[0],'checksum':observed[1],'conditions':'fresh macOS process; one engine thread; Pandas conversion path warmed; data/frame construction excluded from elapsed but retained in baseline RSS; Rust development vs C++ release; not final release acceptance'}
con.close();output.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
