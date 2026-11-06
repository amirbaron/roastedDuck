"""Real binding diagnostics and successful relation calls with keyword/byte arguments."""
import json,re,sys
from pathlib import Path
import duckdb
outcomes={};raw_errors={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as error:
  raw_errors[name]={'error':type(error).__name__,'message':str(error)}
  message=re.sub(r'(object at )0x[0-9a-f]+',r'\1<address>',str(error))
  outcomes[name]={'error':type(error).__name__,'message':message}
def invoke(method,args,kwargs):
 con=duckdb.connect();rel=con.sql('select 42 a');result=getattr(rel,method)(*args,**kwargs)
 return result.fetchall() if result is not None else None
for name,method,args,kwargs in [
 ('execute','execute',(),{}),('execute_arg','execute',('x',),{}),('execute_kw','execute',(),{'sql':'x'}),
 ('query','query',('x','select * from x'),{}),('query_keywords','query',(),{'virtual_table_name':'x','sql_query':'select * from x'}),
 ('query_bytes','query',(b'x',b'select * from x'),{}),('query_bytearray','query',(bytearray(b'x'),bytearray(b'select * from x')),{}),
 ('query_raw_name','query',(b'\xff',b'select 42'),{}),
 ('query_none','query',(None,'select 42'),{}),('query_int','query',('x',42),{}),
 ('query_missing','query',('x',),{}),('query_empty','query',(),{}),('query_extra','query',('x','select 42',1),{}),
 ('query_unknown','query',('x','select 42'),{'unexpected':1}),('query_duplicate','query',('x','select 42'),{'sql_query':'select 43'}),
]:capture(name,lambda method=method,args=args,kwargs=kwargs:invoke(method,args,kwargs))
for name,value in [('none',None),('int',1),('string','x')]:capture('default_'+name,lambda value=value:duckdb.set_default_connection(value))
def default_valid():
 con=duckdb.connect();duckdb.set_default_connection(con);return duckdb.sql('select 43').fetchall()
capture('default_valid',default_valid)
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'raw_errors':raw_errors,'normalization':'only independent Python object addresses; raw errors retained'},indent=2)+'\n');print(json.dumps({'outcomes':len(outcomes)}))
