"""Composed lazy Polars queries and delayed input execution against frozen C++."""
import json,sys,re,gc
from pathlib import Path
import duckdb
import polars as pl
outcomes={};raw={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as e:
  message=str(e);raw[name]={'error':type(e).__name__,'message':message}
  message=message.split('\n\nStack Trace:\n')[0].rstrip() if type(e).__name__=='InternalException' else message
  outcomes[name]={'error':type(e).__name__,'message':re.sub(r'(_duckdb.DuckDBPy(?:Connection|Relation) object at )0x[0-9a-f]+',r'\1<address>',message)}

def frame(owner):
 c=duckdb.connect(config={'threads':1});r=c.sql('select i, i%3 g from range(20)t(i)')if owner=='relation'else c.execute('select i, i%3 g from range(20)t(i)');return c,r.pl(lazy=True)
for owner in ['relation','connection']:
 for name,operation in [
  ('filter',lambda f:f.filter(pl.col('i')>5)),('project_filter',lambda f:f.filter(pl.col('i')>5).select('g')),
  ('filter_head',lambda f:f.filter(pl.col('i')>5).head(3)),('sort_head',lambda f:f.filter(pl.col('g')==1).sort('i',descending=True).head(2)),
  ('concat_self',lambda f:pl.concat([f,f])),('join_self',lambda f:f.join(f,on='i',suffix='_r')),
  ('complex_predicate',lambda f:f.filter((pl.col('i')>3)&(pl.col('i')<9))),
  ('expression_projection',lambda f:f.select((pl.col('i')+3).alias('z'))),
  ('empty_projection',lambda f:f.select(pl.len())),('limit_zero',lambda f:f.head(0)),
 ]:
  def run(owner=owner,operation=operation):
   c,f=frame(owner);return operation(f).collect().to_dicts()
  capture(owner+'_'+name,run)
def input_execution():
 c=duckdb.connect();calls=[]
 def mapper(df):calls.append(df.height);return df
 source=pl.DataFrame({'i':[1,2,3]}).lazy().map_batches(mapper,schema={'i':pl.Int64})
 c.register('v',source);registered=list(calls);rel=c.sql('from v');bound=list(calls);rows=rel.fetchall();return [registered,bound,calls,rows]
capture('lazy_input_deferred',input_execution)
def registered_live():
 c=duckdb.connect();source=pl.DataFrame({'i':[1,2,3]}).lazy();c.register('v',source);return [c.sql('from v').fetchall(),c.sql('from v').fetchall()]
capture('lazy_input_repeatable',registered_live)
for lazy in [False,True]:
 def empty(lazy=lazy):
  c=duckdb.connect();source=pl.DataFrame().lazy()if lazy else pl.DataFrame();c.register('v',source)
 capture('empty_register_'+str(lazy),empty)
for method in ['from_arrow','register']:
 def eager_arrow(method=method):
  c=duckdb.connect();source=pl.DataFrame({'i':[1,2,3]});r=c.from_arrow(source)if method=='from_arrow'else c.register('v',source).sql('from v');return r.fetchall()
 capture('eager_input_'+method,eager_arrow)
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'raw_errors':raw},indent=2)+'\n');print(json.dumps({'cases':len(outcomes)}))
