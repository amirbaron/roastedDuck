"""Frozen table-update validation, expressions, transaction and runtime-result behavior."""
from collections import UserDict
import json
from pathlib import Path
import sys
import duckdb
from duckdb import ConstantExpression as V, ColumnExpression as C
outcomes={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as error:outcomes[name]={'error':type(error).__name__,'message':str(error)}
def update(assignments,condition=None,kind='table'):
 con=duckdb.connect();con.execute("create table t(i int primary key,s varchar default 'default')");con.execute("insert into t values(1,'a'),(2,'b')")
 rel=con.table('t') if kind=='table' else con.sql('select 1 i')
 result=rel.update(assignments,condition=condition)
 return [result,con.sql('select * from t order by i').fetchall()]
for name,assignments,condition in [
 ('default',{'s':duckdb.DefaultExpression()},C('i')==2),
 ('expression',{'i':C('i')+10},None),('primitive',{'i':3},C('i')==1),
 ('all',{'s':V('雪')},True),('none_match',{'s':V('雪')},False),
 ('mapping',UserDict({'s':V('mapped')}),None),
 ('empty',{},None),('not_dict',21,None),('key',{1:V(2)},None),
 ('value',{'s':{2}},None),('column',{'missing':1},None),('condition',{'i':3},{2}),
 ('string_condition',{'s':V('updated')},'i'),
 ('conversion',{'i':V('bad')},None),('constraint',{'i':V(1)},None),
 ('null_constraint',{'i':None},None),
]:capture(name,lambda assignments=assignments,condition=condition:update(assignments,condition))
capture('non_table',lambda:update({'i':V(2)},kind='query'))
def transaction(fail):
 con=duckdb.connect();con.execute('create table t(i int primary key)');con.execute('insert into t values(1),(2)');rel=con.table('t');con.begin()
 result=rel.update({'i':V(1) if fail else C('i')+10})
 try:inside=con.sql('select * from t order by i').fetchall()
 except Exception as e:inside=[type(e).__name__,str(e)]
 con.rollback();return [result,inside,con.sql('select * from t order by i').fetchall()]
for fail in [False,True]:capture('transaction_'+str(fail),lambda fail=fail:transaction(fail))
def qualified():
 con=duckdb.connect();con.execute('create schema s');con.execute('create table s."雪"("a.b" int,"x\"\"y" varchar)');con.execute('insert into s."雪" values(1,\'a\')');rel=con.table('s."雪"');rel.update({'a.b':V(42),'x"y':V('b')});return rel.fetchall()
capture('qualified',qualified)
def suggestion(columns,target):
 con=duckdb.connect();con.execute('create table suggestions('+','.join('"'+c.replace('"','""')+'" int'for c in columns)+')');con.table('suggestions').update({target:V(1)})
for columns,target in [(['native雪','nearby','plain'],'natve雪'),(['longcolumn','short','something'],'longcolum'),(['"quote','a.b','a b'],'a .b')]:
 capture('suggestion_'+target,lambda columns=columns,target=target:suggestion(columns,target))

Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes},indent=2,default=str)+'\n');print(json.dumps({'outcomes':len(outcomes)}))
