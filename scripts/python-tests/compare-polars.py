"""Polars eager/lazy native Arrow conversion, query state and lifetime journeys."""
from pathlib import Path
import json,sys,gc,re,weakref,os
import duckdb
import polars as pl
outcomes={};raw={}
def clean(v):
 if isinstance(v,pl.DataFrame):return [v.columns,[str(t)for t in v.dtypes],v.to_dicts(),v.n_chunks()]
 if isinstance(v,(list,tuple)):return [clean(x)for x in v]
 if isinstance(v,dict):return {str(k):clean(x)for k,x in v.items()}
 if hasattr(v,'isoformat'):return v.isoformat()
 from decimal import Decimal
 if isinstance(v,Decimal):return ['Decimal',str(v)]
 return v

def capture(name,action):
 if os.environ.get('POLARS_LIST_ONLY'):outcomes[name]=None;return
 if os.environ.get('POLARS_CASE') and name!=os.environ['POLARS_CASE']:return
 print('case:',name,flush=True)
 try:outcomes[name]={'value':clean(action())}
 except Exception as e:
  message=str(e);raw[name]={'error':type(e).__name__,'message':message}
  message=re.sub(r'(_duckdb.DuckDBPy(?:Connection|Relation) object at )0x[0-9a-f]+',r'\1<address>',message)
  outcomes[name]={'error':type(e).__name__,'message':message}
 if os.environ.get('POLARS_CASE'):
  Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':clean(outcomes),'raw_errors':raw},indent=2,ensure_ascii=False)+'\n');raise SystemExit(0)

queries={'basic':'select i from range(5)t(i)','duplicate':'select 1 a,2 a,3 A','empty':'select i from range(0)t(i)',
 'nested':"select [1,2,NULL] l, {'a':42,'b':'雪'} s, map(['a'],[1]) m",'decimal':'select 12.34::decimal(8,2) d',
 'temporal':"select timestamp '2020-01-02 03:04:05' t,date '2020-01-02' d",'enum':"select 'a'::enum('a','b') e",'null':'select NULL n',
 'json':"select '{\"x\":42}'::json j"}
for name,query in queries.items():
 for mode in ['relation','connection','lazy_relation','lazy_connection']:
  def run(query=query,mode=mode):
   c=duckdb.connect(config={'threads':1});r=c.execute(query)if mode.endswith('connection')else c.sql(query);frame=r.pl(lazy=mode.startswith('lazy'))
   return frame.collect()if mode.startswith('lazy')else frame
  capture(name+'_'+mode,run)
for owner in ['relation','connection']:
 for lazy in [False,True]:
  for batch in [1,3,0,-1]:
   def batching(owner=owner,lazy=lazy,batch=batch):
    c=duckdb.connect();r=c.sql('select i from range(7)t(i)')if owner=='relation'else c.execute('select i from range(7)t(i)');f=r.pl(batch,lazy=lazy);return f.collect()if lazy else f
   capture('batch_'+owner+'_'+str(lazy)+'_'+str(batch),batching)

def lazy_query():
 c=duckdb.connect();r=c.sql("select i, i%3 g, '雪'||i::varchar s from range(20)t(i)");lf=r.pl(lazy=True)
 return [lf.filter(pl.col('i')>5).select('g','i').limit(3).collect(),lf.filter(pl.col('s').str.ends_with('1')).collect()]
capture('lazy_filter_project_limit_fallback',lazy_query)
for owner in ['relation','connection']:
 def twice(owner=owner):
  c=duckdb.connect();r=c.sql('select i from range(5)t(i)')if owner=='relation'else c.execute('select i from range(5)t(i)');f=r.pl(lazy=True)
  return [f.collect(),f.collect()]
 capture('collect_twice_'+owner,twice)
 def invalidate(owner=owner):
  c=duckdb.connect();r=c.sql('select i from range(5)t(i)')if owner=='relation'else c.execute('select i from range(5)t(i)');f=r.pl(lazy=True);c.execute('select 42');return f.collect()
 capture('invalidate_'+owner,invalidate)
 def close(owner=owner):
  c=duckdb.connect();r=c.sql('select i from range(5)t(i)')if owner=='relation'else c.execute('select i from range(5)t(i)');f=r.pl(lazy=True);c.close();return f.collect()
 capture('closed_'+owner,close)
 def deleted(owner=owner):
  c=duckdb.connect();r=c.sql('select i from range(5)t(i)')if owner=='relation'else c.execute('select i from range(5)t(i)');f=r.pl(lazy=True);del r,c;gc.collect();return f.collect()
 capture('deleted_'+owner,deleted)
 def eager_twice(owner=owner):
  c=duckdb.connect();r=c.sql('select i from range(5)t(i)')if owner=='relation'else c.execute('select i from range(5)t(i)');a=r.pl();b=r.pl();return[a,b]
 capture('eager_twice_'+owner,eager_twice)

def sideeffects():
 c=duckdb.connect();seen=[]
 def f(i):seen.append(i);return i
 c.create_function('f',f,[int],int,side_effects=True);rel=c.sql('select f(i) i from range(10)t(i)');lf=rel.pl(lazy=True);before=len(seen);result=lf.collect();return [before,len(seen),result]
capture('lazy_no_execution_at_schema',sideeffects)
def mutation():
 c=duckdb.connect();c.execute('create table t as select 1 i');r=c.sql('from t').pl(lazy=True);c.execute('insert into t values(2)');return r.collect()
capture('late_catalog_data',mutation)
def input_snapshot(lazy):
 c=duckdb.connect();frame=pl.DataFrame({'i':[1,2,3],'s':['a',None,'雪']});original=frame.lazy()if lazy else frame;ref=weakref.ref(original);c.register('v',original);del original;gc.collect();r=c.sql('from v').pl();c.unregister('v');return [r,ref()is None]
for lazy in [False,True]:capture('registered_'+str(lazy),lambda lazy=lazy:input_snapshot(lazy))
for option in [None,1,'yes']:
 for owner in ['relation','connection']:
  def flag(owner=owner,option=option):
   c=duckdb.connect();r=c.sql('select 1 i')if owner=='relation'else c.execute('select 1 i');f=r.pl(lazy=option);return f.collect()if isinstance(f,pl.LazyFrame)else f
  capture('lazy_option_'+owner+'_'+repr(option),flag)
def module():
 c=duckdb.connect();c.execute('select 42 i');return duckdb.pl(connection=c)
capture('module',module)
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':clean(outcomes),'raw_errors':raw,'normalization':'owner repr addresses, Decimal/datetime values and outer tuples only; Polars dtype/chunk counts retained'},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
