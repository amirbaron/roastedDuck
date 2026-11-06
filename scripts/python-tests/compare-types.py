"""Frozen Python type construction, nested metadata and target-conversion journeys."""
import datetime,json,sys,typing
from pathlib import Path
import duckdb
import numpy as np
T=duckdb.sqltypes.DuckDBPyType
outcomes={}
def clean(v):
 if isinstance(v,(list,tuple)):return [clean(x)for x in v]
 if isinstance(v,dict):return {str(k):clean(x)for k,x in v.items()}
 if isinstance(v,T):return str(v)
 return v

def capture(name,action):
 try:outcomes[name]={'value':clean(action())}
 except Exception as e:outcomes[name]={'error':type(e).__name__,'message':str(e)}

specs=[('str',str),('int',int),('float',float),('bool',bool),('bytes',bytes),('bytearray',bytearray),('np_int8',np.int8),('np_uint64',np.uint64),('np_half',np.float16),('none',None),('number',42),('object',object),('date_class',datetime.date),('list',list[str]),('map',dict[str,int]),('dict',{'雪':int,'b':str}),('empty_dict',{}),('union',int|str),('optional',typing.Optional[list[int]]),('optional_map',dict[int|None,str|None]|None),('invalid_generic',tuple[int,str]),('sql_alias','int'),('sql_invalid','bad_type_name')]
for name,spec in specs:
 capture('construct_'+name,lambda spec=spec:str(T(spec)))
 capture('list_'+name,lambda spec=spec:str(duckdb.list_type(spec)))
for sql in ['INTEGER','INTEGER[]','INTEGER[2]','MAP(VARCHAR,INTEGER)',"STRUCT(a INTEGER,\"雪\" VARCHAR)",'UNION(a INTEGER,b VARCHAR)',"ENUM('a','雪')",'DECIMAL(8,2)','JSON']:
 capture('metadata_'+sql,lambda sql=sql:[T(sql).id,T(sql).children])
for sql,name in [('INTEGER','child'),('INTEGER[]','ChIlD'),('INTEGER[2]','child'),('MAP(VARCHAR,INTEGER)','KEY'),('MAP(VARCHAR,INTEGER)','missing'),('STRUCT(a INTEGER)','A'),('STRUCT(a INTEGER)','missing'),('UNION(a INTEGER,b VARCHAR)','')]:
 capture('attribute_'+sql+'_'+name,lambda sql=sql,name=name:[str(getattr(T(sql),name)),str(T(sql)[name])])
for sql,name in [('INTEGER','int'),('INTEGER','integer'),('INTEGER',' INTEGER '),('VARCHAR','text'),('JSON','json')]:
 capture('equal_'+sql+'_'+name,lambda sql=sql,name=name:[T(sql)==name,hash(T(sql))==hash(str(T(sql)))])
for value,target in [(42,'UNION(a VARCHAR,b BIGINT,c BIGINT)'),(1.5,'UNION(a DOUBLE,b DOUBLE)'),('hello','UNION(a VARCHAR,b VARCHAR)'),(True,'UNION(a BOOLEAN,b BIGINT)'),((42,'yes'),'STRUCT(a BIGINT,b VARCHAR)'),((42,),'STRUCT(a BIGINT,b VARCHAR)'),({'a':42,'extra':2},'STRUCT(a BIGINT)')]:
 def parameter(value=value,target=target):
  con=duckdb.connect();return con.sql('select $1',params=[duckdb.Value(value,T(target))]).fetchall()
 capture('parameter_'+repr(value)+'_'+target,parameter)
for name,value,target in [
 ('float_duplicate',1.5,'UNION(a DOUBLE,b DOUBLE)'),('int_duplicate',42,'UNION(a VARCHAR,b BIGINT,c BIGINT)'),
 ('str_duplicate','hello','UNION(a VARCHAR,b VARCHAR)'),('bool_union',True,'UNION(a BOOLEAN,b BIGINT)'),
 ('int_no_exact',42,'UNION(a VARCHAR,b SMALLINT)'),('float_no_exact',1.5,'UNION(a VARCHAR,b BIGINT)'),
 ('nested_float',{'v':1.5},'STRUCT(v UNION(a DOUBLE,b DOUBLE))'),
 ('nested_int',{'v':42},'STRUCT(v UNION(a VARCHAR,b BIGINT,c BIGINT))'),
 ('tuple',(42,'yes'),'STRUCT(a BIGINT,b VARCHAR)'),
]:
 def udf(value=value,target=target):
  con=duckdb.connect();con.create_function('f',lambda:value,[],T(target));return con.sql('select f()').fetchall()
 capture('udf_'+name,udf)
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
