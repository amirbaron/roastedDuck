"""Dataframe callback journeys over native query trees, compared with pinned C++ DuckDB."""
import gc,json,math,sys,weakref,os
from datetime import timedelta
from decimal import Decimal
from uuid import UUID
from pathlib import Path
import duckdb
import pandas as pd

outcomes={}
def clean(value):
 if isinstance(value,float) and math.isnan(value):return 'NaN'
 if isinstance(value,(list,tuple)):return [clean(v) for v in value]
 if isinstance(value,dict):return {str(k):clean(v)for k,v in value.items()}
 if isinstance(value,(bytes,bytearray)):return ['bytes',bytes(value).hex()]
 if hasattr(value,'isoformat'):return value.isoformat()
 if isinstance(value,timedelta):return ['timedelta',value.days,value.seconds,value.microseconds]
 if isinstance(value,(Decimal,UUID)):return [type(value).__name__,str(value)]
 return value

def capture(name,action):
 if os.environ.get('DUCKDB_MAP_CASE') and name!=os.environ['DUCKDB_MAP_CASE']:return
 print('case:',name,flush=True)
 try:outcomes[name]={'value':clean(action())}
 except Exception as error:outcomes[name]={'error':type(error).__name__,'message':str(error)}
 Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes},indent=2)+'\n')
 if os.environ.get('DUCKDB_MAP_CASE'):raise SystemExit(0)

def describe(rel):return [rel.type,rel.columns,[str(t) for t in rel.types],rel.fetchall()]

def mapping(query,function=lambda df:df,schema=None):
 con=duckdb.connect(config={'threads':1});return describe(con.sql(query).map(function,schema=schema))

for name,query in {
 'integers':'select i::integer i from range(5)t(i)',
 'blob':"select 'abc'::blob b",'strings':"select * from (values ('雪'),(NULL),('a'))t(s)",
 'list':'select [1,2,NULL] a','struct':"select {'a':42,'雪':'yes'} a",'map':"select map(['a'],[1]) a",
 'boolean':'select true b','null':'select NULL a','nullable_integer':'select * from (values (1),(NULL))t(i)',
 'float':'select 1.25::float f','decimal':'select 12.34::decimal(5,2) d',
 'date':"select date '2020-01-02' d",'timestamp':"select timestamp '2020-01-02 03:04:05' t",
 'timestamptz':"select timestamptz '2020-01-02 03:04:05+00' t",'interval':"select interval '1 day' i",
 'enum':"select 'a'::enum('a','b') e",'uuid':"select '00000000-0000-0000-0000-000000000001'::uuid u",
 'empty':'select i from range(0)t(i)',
}.items():
 capture('identity_'+name,lambda query=query:mapping(query))
 # Return the observed dtype and first scalar's Python type, regardless of inferred input type.
 def inspect(query=query):
  def mapper(df):
   return pd.DataFrame({'description': [str(df.dtypes.iloc[0])+';'+(type(df.iloc[0,0]).__name__ if len(df) else 'empty')]*len(df)})
  return mapping(query,mapper,{'description':str})
 capture('input_'+name,inspect)

for kind in ['BOOLEAN','TINYINT','SMALLINT','INTEGER','BIGINT','UTINYINT','USMALLINT','UINTEGER','UBIGINT','FLOAT','DOUBLE','TIMESTAMP_S','TIMESTAMP_MS','TIMESTAMP_NS','TIMESTAMP','TIMESTAMPTZ']:
 def numeric_input(kind=kind):
  base="timestamp '2020-01-02 03:04:05'+i*interval '1 microsecond'" if kind.startswith('TIMESTAMP') else 'i'
  query=f"select (case when i%3=0 then NULL else {base} end)::{kind} v from range(7)t(i) where i%2=0"
  def mapper(df):return pd.DataFrame({'v':[str(v)for v in df['v']]})
  return mapping(query,mapper,{'v':str})
 capture('nullable_input_'+kind,numeric_input)
capture('constant_numeric',lambda:mapping('select 42::bigint i from range(5)'))
capture('dictionary_numeric',lambda:mapping('select i::smallint i from range(10)t(i) where i%2=0'))

capture('project_filter_chain',lambda: describe(duckdb.sql('select i from range(6)t(i)').map(lambda df:df.assign(j=df.i+1)).filter('j>3').project('sum(j) s'))) # C++ project rejects aggregate
capture('native_aggregate_chain',lambda:describe(duckdb.sql('select i from range(6)t(i)').map(lambda df:df.assign(j=df.i+1)).filter('j>3').aggregate('sum(j) s')))
capture('drop_rows',lambda:mapping('select i from range(6)t(i)',lambda df:df[df.i%2==0]))
capture('expand_rows',lambda:mapping('select 1::bigint i',lambda df:pd.concat([df]*3)))
capture('zero_rows',lambda:mapping('select 1::bigint i',lambda df:df.iloc[0:0]))
capture('schema_order',lambda:mapping('select 1::bigint i,2::bigint j',schema={'j':int,'i':int}))
capture('schema_sql_strings',lambda:mapping('select 1::bigint i',schema={'i':'BIGINT'}))
capture('schema_native_types',lambda:mapping('select 1::bigint i',schema={'i':duckdb.sqltypes.BIGINT}))
capture('schema_invalid_type',lambda:mapping('select 1 i',schema={'i':42}))
capture('schema_empty',lambda:mapping('select 1 i',schema={}))
capture('schema_list',lambda:mapping('select 1 i',schema=[int]))
capture('renamed_null_type',lambda:mapping("select 'x' s",lambda df:df.rename(columns={'s':'renamed'})))
capture('return_series',lambda:mapping('select 1 i',lambda df:df['i']))
capture('return_none',lambda:mapping('select 1 i',lambda df:None))
capture('return_empty_columns',lambda:mapping('select 1 i',lambda df:pd.DataFrame()))
capture('return_arrow',lambda:mapping('select 1 i',lambda df:df.convert_dtypes(dtype_backend='pyarrow')))
capture('return_too_many_rows',lambda:mapping('select 1::bigint i',lambda df:pd.DataFrame({'i':range(2049)}),{'i':int}))
def raises(df):raise ValueError('map callback traceback sentinel')
capture('callback_error',lambda:mapping('select 1 i',raises))

def traceback_state():
 for name in ['last_type','last_value','last_traceback','last_exc']:
  if hasattr(sys,name):delattr(sys,name)
 try:mapping('select 1 i',raises)
 except duckdb.InvalidInputException:pass
 return [sys.last_type.__name__,str(sys.last_value),type(sys.last_exc).__name__,sys.last_traceback is not None]
capture('callback_traceback_state',traceback_state)
def function_catalog():
 return duckdb.sql("select parameter_types from duckdb_functions() where function_name='python_map_function'").fetchall()
capture('native_function_catalog',function_catalog)


def explicit_no_inference():
 seen=[]
 def mapper(df):seen.append(len(df));return df
 con=duckdb.connect(config={'threads':1});rel=con.sql('select i from range(5)t(i)').map(mapper,schema={'i':int})
 before=list(seen);rows=rel.fetchall();return [before,seen,rows]
capture('explicit_no_inference',explicit_no_inference)

def batches():
 seen=[]
 def mapper(df):
  if len(df):seen.append(len(df))
  return df.assign(i=df.i+1)
 con=duckdb.connect(config={'threads':1});rel=con.sql('select i from range(10001)t(i)').map(mapper)
 result=rel.aggregate('count(*) n,sum(i) s').fetchall()
 return [result,sum(seen),max(seen),sorted(seen)]
capture('bounded_batches',batches)

def retained_batch():
 seen=[];latest=[None]
 def mapper(df):
  if len(df):seen.append(len(df))
  latest[0]=df.assign(i=df.i+1)
  return latest[0]
 con=duckdb.connect(config={'threads':1});rel=con.sql('select i from range(10001)t(i)').map(mapper)
 result=rel.aggregate('count(*) n,sum(i) s').fetchall()
 return [result,sum(seen),max(seen),sorted(seen)]
capture('bounded_batches_retained_output',retained_batch)

def empty_inference():
 seen=[]
 def mapper(df):seen.append(len(df));return df
 con=duckdb.connect();rel=con.sql('select i from range(0)t(i)').map(mapper);rows=rel.fetchall()
 return [seen,rows]
capture('empty_input_inference',empty_inference)

def error_recovery():
 con=duckdb.connect()
 for explicit in [False,True]:
  try:con.sql('select 1::bigint i').map(raises,schema={'i':int}if explicit else None).fetchall()
  except duckdb.InvalidInputException:pass
 return con.sql('select 42').fetchall()
capture('callback_error_recovery',error_recovery)

for schema in [{'i':'invalid_type_name'},{'i':None},{'i':object}]:
 capture('invalid_schema_'+str(next(iter(schema.values()))),lambda schema=schema:mapping('select 1 i',schema=schema))


def lifetime():
 con=duckdb.connect();frame=pd.DataFrame({'i':[1,2,3]});frame_ref=weakref.ref(frame)
 class Mapper:
  def __call__(self,df):return df.assign(i=df.i+10)
 callback=Mapper();callback_ref=weakref.ref(callback)
 rel=con.from_df(frame).map(callback);del callback,frame;gc.collect()
 retained=[callback_ref() is not None,frame_ref() is not None]
 rel.create_view('mapped');del rel;gc.collect();rows=con.sql('select * from mapped order by i').fetchall()
 con.execute('drop view mapped');gc.collect()
 return [retained,rows,callback_ref() is None,frame_ref() is None]
capture('retained_view_and_release',lifetime)

def rollback():
 con=duckdb.connect();con.execute('create table t(i bigint)');con.execute('begin');con.execute('insert into t values (7)')
 try:con.sql('select i from t').map(raises,schema={'i':int}).fetchall()
 except duckdb.InvalidInputException:pass
 con.execute('rollback');return con.sql('select * from t').fetchall()
capture('error_transaction_rollback',rollback)

Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'normalization':'NaN, bytes and datetime values only; every error retained'},indent=2)+'\n')
print(json.dumps({'outcomes':len(outcomes)}))
