"""Native append registration, transaction, conversion and call-contract journeys."""
import gc,json,re,sys,weakref
from pathlib import Path
import duckdb,numpy as np,pandas as pd
outcomes={};raw={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as e:
  raw[name]={'error':type(e).__name__,'message':str(e)}
  outcomes[name]={'error':type(e).__name__,'message':re.sub(r'(_duckdb.DuckDBPy(?:Connection|Relation) object at )0x[0-9a-f]+',r'\1<address>',str(e))}
for module in [False,True]:
 for by_name in [False,True]:
  def run(module=module,by_name=by_name):
   c=duckdb.connect();c.execute("create table t(a integer,b integer default 9)");df=pd.DataFrame({'b':[2,3],'a':[5,6]});r=duckdb.append('t',df,by_name=by_name,connection=c)if module else c.append('t',df,by_name=by_name);count=r.fetchall();return [r is c,count,c.sql('from t').fetchall(),c.sql('from __append_df').fetchall()]
  capture(f'normal_{module}_{by_name}',run)
for name in ['normal','table','snow雪','a.b','x";DROP TABLE t;--']:
 def run(name=name):
  c=duckdb.connect();q='"'+name.replace('"','""')+'"';c.execute(f'create table {q}(i bigint)');c.append(name,pd.DataFrame({'n':[1,None,3]}));return c.sql(f'from {q}').fetchall()
 capture('name_'+name,run)
for name,df,by_name in [('defaults',pd.DataFrame({'a':[1,2]}),True),('missing',pd.DataFrame({'unknown':[1]}),True),('duplicate',pd.DataFrame([[1,2]],columns=['a','a']),True),('width',pd.DataFrame({'a':[1]}),False),('bad_cast',pd.DataFrame({'a':['bad'],'b':[2]}),False),('empty',pd.DataFrame({'a':pd.Series([],dtype='int64'),'b':pd.Series([],dtype='int64')}),False)]:
 def run(df=df,by_name=by_name):
  c=duckdb.connect();c.execute('create table t(a integer,b integer default 9)');c.append('t',df,by_name=by_name);return [c.fetchall(),c.sql('from t').fetchall()]
 capture('data_'+name,run)
def repeated():
 c=duckdb.connect();c.execute('create table t(a bigint)');c.append('t',pd.DataFrame({'a':[1,2]}));c.append('t',pd.DataFrame({'a':[3]}));return [c.sql('from t').fetchall(),c.sql('from __append_df').fetchall()]
capture('repeated',repeated)
def registered():
 c=duckdb.connect();c.execute('create table t(a bigint)');df=pd.DataFrame({'a':[1,2]});c.append('t',df);df.iloc[0,0]=42;del df;gc.collect();return [c.sql('from t').fetchall(),c.sql('from __append_df').fetchall()]
capture('retained_live',registered)
def rollback():
 c=duckdb.connect();c.execute('create table t(a bigint)');c.begin();c.append('t',pd.DataFrame({'a':[1,2]}));c.rollback();return c.sql('from t').fetchall()
capture('rollback',rollback)
def failure_registration():
 c=duckdb.connect();df=pd.DataFrame({'a':[1]})
 try:c.append('missing',df)
 except duckdb.CatalogException:pass
 return c.sql('from __append_df').fetchall()
capture('failed_insert_retains_registration',failure_registration)
def conflict():
 c=duckdb.connect();c.execute('create table t(a bigint)');c.sql('select 42 a').create_view('__append_df');c.append('t',pd.DataFrame({'a':[1]}));return c.sql('from t').fetchall()
capture('user_view_conflict',conflict)
for module in [False,True]:
 for label,args,kwargs in [('missing',(),{}),('no_df',('t',),{}),('extra',('t',pd.DataFrame({'a':[1]}),True),{}),('bad_df',('t',None),{}),('bad_name',(42,pd.DataFrame({'a':[1]})),{}),('duplicate',('t',pd.DataFrame({'a':[1]})),{'table_name':'t'}),('unknown',('t',pd.DataFrame({'a':[1]})),{'bad':1}),('bytes',(b't',pd.DataFrame({'a':[1]})),{})]:
  def run(module=module,args=args,kwargs=kwargs):
   c=duckdb.connect();c.execute('create table t(a bigint)');r=duckdb.append(*args,connection=c,**kwargs)if module else c.append(*args,**kwargs);return [r is c,c.sql('from t').fetchall()]
  capture(f'call_{module}_{label}',run)
 for value in [None,0,1,'yes',np.bool_(True)]:
  def run(value=value,module=module):
   c=duckdb.connect();c.execute('create table t(a bigint)');df=pd.DataFrame({'a':[1]});r=duckdb.append('t',df,by_name=value,connection=c)if module else c.append('t',df,by_name=value);return [r is c,c.sql('from t').fetchall()]
  capture(f'bool_{module}_{type(value).__name__}_{value}',run)
for parent in [False,True]:
 for method in ['interrupt','query_progress','append']:
  def run(parent=parent,method=method):
   c=duckdb.connect();child=c.cursor();target=child if parent else c;c.close();args=('t',pd.DataFrame({'a':[1]}))if method=='append'else();return getattr(target,method)(*args)
  capture(f'closed_{parent}_{method}',run)
Path(sys.argv[1]).write_text(json.dumps({'outcomes':outcomes,'raw_errors':raw},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
