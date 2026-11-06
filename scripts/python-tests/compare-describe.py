"""Native descriptive aggregate relation values, types, laziness and input ownership."""
import json
from pathlib import Path
import sys
import duckdb
outcomes={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as error:outcomes[name]={'error':type(error).__name__,'message':str(error)}
def describe(query):
 con=duckdb.connect();rel=con.sql(query);described=rel.describe()
 return [described.type,described.columns,[str(t)for t in described.types],described.fetchall()]
for name,query in {
 'one':'select 42::int a,84::bigint b',
 'numeric':'select i, i*10 j, i/4.0 k from range(11)t(i)',
 'nulls':'select case when i%2=0 then i else NULL end i from range(11)t(i)',
 'empty':'select 42 a limit 0',
 'strings':"select 'hello' a, true b, NULL::int c",
 'nested':"select [1,NULL,3] a, {'x':1,'y':'b'} b, MAP(['a'],[2]) c",
 'temporal':"select timestamp '1992-01-01' a, date '2000-01-01' b, time '12:34:56' c",
 'decimal':"select i::decimal(10,2) d from range(11)t(i)",
 'quoted':"select 42 as \"a.b\", 1 as \"a\"\"b\", 2 as \"雪\"",
 'duplicate':"select 1 a, 2 a",
 'aggr_name':"select 42 aggr",
}.items():capture(name,lambda query=query:describe(query))
def lazy():
 con=duckdb.connect();seen=[]
 def f(x:int)->int:seen.append(x);return x
 con.create_function('f',f,side_effects=True)
 rel=con.sql('select f(i) a from range(7)t(i)').describe()
 before=len(seen);rows=rel.fetchall()
 return [before,len(seen),rows]
capture('lazy',lazy)
def stream():
 con=duckdb.connect();con.execute('select i from range(5000)t(i)');first=con.fetchone()
 rel=con.sql('select 42 a').describe();after=con.fetchone()
 return [first,after,rel.fetchall()]
capture('metadata_preserves_stream',stream)
def owner():
 import pyarrow as pa
 con=duckdb.connect();table=pa.table({'i':[1,2,3]});rel=con.from_arrow(table).describe();del table
 return rel.fetchall()
capture('retained_arrow',owner)
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes},indent=2,default=str)+'\n');print(json.dumps({'outcomes':len(outcomes)}))
