"""Repeat real Polars scans with shared and independent native connections."""
import collections,json,pathlib,sys
import duckdb,polars as pl
scenario=sys.argv[1];runs=[]
for index in range(100):
 c=duckdb.connect(config={'threads':1});r=c.sql('select i,i%3 g from range(20)t(i)');f=r.pl(lazy=True)
 if scenario=='same_relation':other=f
 elif scenario=='same_connection':other=c.sql('select i,i%3 g from range(20)t(i)').pl(lazy=True)
 elif scenario=='independent':second=duckdb.connect(config={'threads':1});other=second.sql('select i,i%3 g from range(20)t(i)').pl(lazy=True)
 else:other=f
 try:
  if scenario=='concat':
   rows=pl.concat([f,other]).collect().sort('i').to_dicts();expected=sorted([{'i':i,'g':i%3}for i in range(20)]*2,key=lambda x:x['i'])
  else:rows=f.join(other,on='i',suffix='_r').collect().sort('i').to_dicts();expected=[{'i':i,'g':i%3,'g_r':i%3}for i in range(20)]
  assert rows==expected,(rows,expected);runs.append({'correct_rows':len(rows)})
 except Exception as e:runs.append({'error':type(e).__name__,'message':str(e)})
 c.close()
 if scenario=='independent':second.close()
pathlib.Path(sys.argv[2]).write_text(json.dumps({'scenario':scenario,'runs':runs,'summary':dict(collections.Counter(json.dumps(x,sort_keys=True)for x in runs))},indent=2)+'\n')
