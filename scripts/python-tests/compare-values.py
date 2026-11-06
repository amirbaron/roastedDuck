"""Value-relation ASTs and table insertion, including target identity and transactions."""
import json
from pathlib import Path
import sys
import duckdb
from duckdb import ConstantExpression as V, ColumnExpression as C
outcomes={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as error:outcomes[name]={'error':type(error).__name__,'message':str(error)}
def values(*args):
 con=duckdb.connect();rel=con.values(*args)
 return [rel.type,rel.alias,rel.columns,[str(x)for x in rel.types],rel.sql_query(),rel.fetchall()]
for name,args in {
 'list':([1,2,3],),'null':([None,'雪',True],),'empty_args':(),'empty_list':([],),
 'ints':(1,2,3),'column':('missing',),'expression':(V('a'),V(2),V(None)),
 'mixed':(V('1'),2,V('3')),'tuples':((V(1),V(2)),(V(3),V(4))),
 'duplicates':((V(1),V(1)),),'alias':(V(1).alias('雪'),),'empty_tuple':((),),
 'mismatch':((V(1),V(2)),(V(3),)),'tuple_expr':((V(1),),V(2)),
 'bad':(V(1),{1}),'list_expression':([V(1)],),'nested':([[1,2],{'x':3}],),
}.items():capture(name,lambda args=args:values(*args))

def insert(kind='table',values=None):
 con=duckdb.connect();con.execute('create table items(i int primary key,s varchar default \'default\')')
 rel=con.table('items') if kind=='table' else con.sql('select 1 i')
 rel.insert([1,'雪'] if values is None else values)
 return con.sql('select * from items').fetchall()
for name,kind,value in [('insert','table',None),('tuple_insert','table',(2,'a')),('wrong_count','table',[1]),('wrong_type','table',['bad','a']),('query_insert','query',[1]),('empty_insert','table',[])]:capture(name,lambda kind=kind,value=value:insert(kind,value))

def duplicate():
 con=duckdb.connect();con.execute('create table items(i int primary key)');rel=con.table('items');rel.insert([1])
 try:rel.insert([1])
 except duckdb.ConstraintException:pass
 return rel.fetchall()
capture('constraint_recovery',duplicate)

def rollback():
 con=duckdb.connect();con.execute('create table items(i int)');rel=con.table('items');con.begin();rel.insert([1]);con.rollback();return rel.fetchall()
capture('rollback',rollback)

def target(scope):
 con=duckdb.connect();con.execute('create table items(i int)');rel=con.table('items')
 if scope=='catalog':
  con.execute("attach ':memory:' as other");con.execute('create table other.items(i int)');con.execute('use other')
  rel.insert([42]);return [con.sql('select * from memory.items').fetchall(),con.sql('select * from other.items').fetchall()]
 con.execute('create schema s');con.execute('create table s.items(i int)');con.execute("set search_path='s'")
 rel=con.table('items');rel.insert([42]);return [con.sql('select * from main.items').fetchall(),con.sql('select * from s.items').fetchall()]
for scope in ['catalog','schema']:capture('target_'+scope,lambda scope=scope:target(scope))
def insertion_failure(kind):
 con=duckdb.connect();con.execute('create table items(i int primary key)');rel=con.table('items');rel.insert([1])
 if kind=='missing':con.execute('drop table items')
 if kind=='aborted':
  con.begin()
  try:rel.insert([1])
  except duckdb.ConstraintException:pass
 rel.insert([1 if kind=='duplicate' else 2])
for kind in ['duplicate','missing','aborted']:capture('error_'+kind,lambda kind=kind:insertion_failure(kind))

Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes},indent=2,default=str)+'\n');print(json.dumps({'outcomes':len(outcomes)}))
