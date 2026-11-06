"""Frozen root dataframe overloads, explicit values and closed-result consumers."""
import datetime,decimal,json,re,sys
from pathlib import Path
import duckdb,pandas as pd
outcomes={};raw={}
def capture(name,action):
 try:
  value=action()
  if isinstance(value,duckdb.DuckDBPyRelation):value={'rows':repr(value.fetchall()),'types':[str(t)for t in value.types]}
  elif isinstance(value,pd.DataFrame):value={'frame':value.to_dict('list'),'types':[str(t)for t in value.dtypes]}
  else:value=repr(value)
  outcomes[name]={'value':value}
 except Exception as e:
  raw[name]={'error':type(e).__name__,'message':str(e)}
  outcomes[name]={'error':type(e).__name__,'message':re.sub(r'(_duckdb.DuckDBPy(?:Connection|Relation) object at )0x[0-9a-f]+',r'\1<address>',str(e))}
df=pd.DataFrame({'a':[1,2,3],'g':['x','x','y']})
for name,args,kwargs in [('fetch',(),{}),('fetch_date',(),{'date_as_object':True}),('fetch_none',(),{'date_as_object':None}),('fetch_numeric',(),{'date_as_object':2}),('fetch_string',(),{'date_as_object':'yes'}),('input',(df,),{}),('keyword',(),{'df':df}),('none',(None,),{}),('bool',(False,),{}),('list',([1],),{}),('input_date',(df,),{'date_as_object':False}),('duplicate',(df,),{'df':df}),('extra',(df,df),{}),('unknown',(),{'bad':True})]:
 def run(args=args,kwargs=kwargs):
  c=duckdb.connect();c.execute('select 42 answer');return duckdb.df(*args,connection=c,**kwargs)
 capture('df_'+name,run)
for method,args,kwargs in [('aggregate',(df,'sum(a)'),{}),('aggregate',(df,'sum(a)','g'),{}),('aggregate',(),{'df':df,'aggr_expr':'sum(a)','group_expr':'g'}),('aggregate',(None,'sum(a)'),{}),('limit',(df,2),{}),('limit',(df,1,1),{}),('limit',(),{'df':df,'n':1,'offset':2}),('limit',(df,-1),{}),('limit',(None,1),{})]:
 capture(method+'_'+str(len(outcomes)),lambda method=method,args=args,kwargs=kwargs:getattr(duckdb,method)(*args,**kwargs))
for target in ['BIT','DECIMAL(12,8)','TIMESTAMP_S','TIMESTAMP_MS','TIMESTAMP_NS','TIMESTAMP','TIMESTAMPTZ']:
 values=[b'0101',bytearray(b'0101'),b'',b'0x',b'\xff']if target=='BIT'else [1.2345,0.0,float('nan'),float('inf'),decimal.Decimal('1.2345')]if target.startswith('DECIMAL')else [datetime.datetime(2001,2,3,4,5,6,789012),datetime.datetime(2001,2,3,4,5,6,789012,tzinfo=datetime.timezone(datetime.timedelta(hours=2))),None]
 for i,value in enumerate(values):
  def run(target=target,value=value):
   c=duckdb.connect();v=duckdb.Value(value,duckdb.sqltype(target));return c.execute('select $1',[v]).fetchall()
  capture('typed_'+target+'_'+str(i),run)
for method in ['fetchone','fetchall','fetchnumpy','to_arrow_table','to_arrow_reader','arrow','pl','df']:
 def run(method=method):
  c=duckdb.connect();r=c.sql('select 42 a').execute();r.close();return getattr(r,method)()
 capture('closed_'+method,run)
Path(sys.argv[1]).write_text(json.dumps({'outcomes':outcomes,'raw_errors':raw},indent=2,ensure_ascii=True)+'\n');print(json.dumps({'cases':len(outcomes)}))
