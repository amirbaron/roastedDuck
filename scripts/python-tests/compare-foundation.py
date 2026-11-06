"""Native Rust-vs-frozen-C++ Python outcomes. Run separately in each isolated environment."""
from pathlib import Path
import datetime,decimal,hashlib,json,math,sys,uuid
import duckdb

def value(obj):
 if isinstance(obj,bytes):return {'bytes':obj.hex()}
 if isinstance(obj,(datetime.date,datetime.time,datetime.datetime)):return {type(obj).__name__:obj.isoformat()}
 if isinstance(obj,datetime.timedelta):return {'timedelta':[obj.days,obj.seconds,obj.microseconds]}
 if isinstance(obj,(decimal.Decimal,uuid.UUID)):return {type(obj).__name__:str(obj)}
 if isinstance(obj,float) and not math.isfinite(obj):return {'float':str(obj)}
 if isinstance(obj,(list,tuple)):return [value(v)for v in obj]
 if isinstance(obj,dict):return {'dict':[[value(k),value(v)]for k,v in obj.items()]}
 return obj
report={'version':duckdb.__version__,'module':duckdb.__file__,'outcomes':{}}
def capture(name,action):
 try:report['outcomes'][name]={'value':value(action())}
 except Exception as error:report['outcomes'][name]={'error':type(error).__name__,'message':str(error)}
with duckdb.connect() as connection:
 queries={
  'scalars':"SELECT true,NULL::INTEGER,42::BIGINT,3.25::DOUBLE,'雪',from_hex('610062ff')",
  'large_ints':"SELECT 9223372036854775808::UBIGINT,170141183460469231731687303715884105727::HUGEINT,340282366920938463463374607431768211455::UHUGEINT",
  'decimal':"SELECT 0.001::DECIMAL(9,3),123.00::DECIMAL(9,2)",
  'dates':"SELECT DATE '2000-02-29',DATE 'infinity',DATE '-infinity'",
  'timestamps':"SELECT TIMESTAMP '2024-01-02 03:04:05.123456',TIMESTAMP_NS '1969-12-31 23:59:59.999999999'",
  'times':"SELECT TIME '03:04:05.123456',TIMETZ '03:04:05.123456+02:30'",
  'interval':"SELECT INTERVAL '1 month 2 days 3 microseconds'",
  'uuid':"SELECT '550e8400-e29b-41d4-a716-446655440000'::UUID",
  'nested':"SELECT [1,NULL,3],{'a':42,'b':['x','雪']},MAP(['a','b'],[1,NULL])",
  'float_special':"SELECT 'NaN'::DOUBLE,'Infinity'::DOUBLE,'-Infinity'::DOUBLE",
 }
 for name,sql in queries.items():capture(name,lambda sql=sql:connection.execute(sql).fetchall())
 for index,parameter in enumerate([None,True,42,-(1<<63),1<<63,1<<64,1<<127,(1<<128)-1,1<<128,-(1<<127)-1,float('nan'),'雪',b'a\0b\xff',[1,'a'],{'a':1},decimal.Decimal('12.30'),datetime.date(2024,1,2),datetime.datetime(2024,1,2,3,4,5,123456),datetime.time(3,4,5,123456,tzinfo=datetime.timezone(datetime.timedelta(hours=2))),datetime.timedelta(days=2,microseconds=3),uuid.UUID('550e8400-e29b-41d4-a716-446655440000')]):
  capture('parameter_'+str(index),lambda parameter=parameter:connection.execute('SELECT typeof(?), ?',[parameter,parameter]).fetchall())
 capture('description',lambda:[tuple(str(item)if i==1 else item for i,item in enumerate(row))for row in connection.execute('SELECT 42 AS answer').description])
 for name,sql in [('parser','SELECT FROM'),('catalog','SELECT * FROM missing_table'),('invalid_input',"SELECT error('host failure')")]:capture('error_'+name,lambda sql=sql:connection.execute(sql).fetchall())
 capture('commit_without_transaction',lambda:connection.commit() is connection)
 connection.execute('SELECT * FROM range(5)');capture('incremental_fetch',lambda:[connection.fetchone(),connection.fetchmany(2),connection.fetchall(),connection.fetchone()])
def parent_close():
 parent=duckdb.connect();child=parent.cursor();parent.close()
 try:child.execute('SELECT 1')
 except Exception as error:return [type(error).__name__,str(error)]
 raise AssertionError('child remained open')
capture('parent_cursor_close',parent_close)
def default_reopen():
 connection=duckdb.connect();duckdb.set_default_connection(connection);duckdb.close();new=duckdb.default_connection();new.execute('SELECT 42');result=new.fetchone();duckdb.close();return result
capture('default_reopen',default_reopen)
def timezone_result():
 with duckdb.connect() as con:
  cursor=con.cursor();con.execute("SET GLOBAL timezone='UTC'");return str(cursor.execute('SELECT make_timestamptz(2000,1,20,3,30,59)').fetchone())
capture('timezone_representation',timezone_result)
connection.close();capture('closed',lambda:connection.execute('SELECT 1'));capture('closed_fetch',lambda:connection.fetchall())
Path(sys.argv[1]).write_text(json.dumps(report,indent=2,ensure_ascii=True)+'\n');print(json.dumps({'outcomes':len(report['outcomes']),'module':report['module']}))
