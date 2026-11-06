"""Native statement ownership, parsing, discovery, execution and extension lifecycle."""
import json,pathlib,pickle,re,sys
import duckdb
outcomes={};raw={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as e:
  message=str(e);raw[name]={'error':type(e).__name__,'message':message}
  if type(e).__name__=='InternalException':message=message.split('\n\nStack Trace:\n')[0].rstrip()
  message=re.sub(r'(_duckdb\.(?:DuckDBPyConnection|Statement) object at )0x[0-9a-f]+',r'\1<address>',message)
  outcomes[name]={'error':type(e).__name__,'message':message}
def metadata(sql):
 c=duckdb.connect();return [{'query':s.query,'type':[s.type.name,int(s.type),str(s.type),repr(s.type)],'parameters':sorted(s.named_parameters),'expected':[(v.name,int(v),str(v),repr(v))for v in s.expected_result_type]}for s in c.extract_statements(sql)]
for sql in ['', '-- hello','select 42',' select $1; select $Name + $name;  ','select $雪 as "é"','select :bad','select 1; -- tail\n','create table t(i int)','insert into t values($1)','update t set i=3','delete from t','copy t to \'file\'','attach \'file\' as x','detach x','begin','commit','set threads=1','pragma version','pragma bad_pragma','call pragma_version()','prepare s as select $1','execute s(1)','explain select 42','vacuum','analyze t','export database \'file\'','load json','install json','update extensions','merge into t using s on t.i=s.i when matched then delete','select 1; select * from missing']:
 capture('meta_'+sql,lambda sql=sql:metadata(sql))
for cls in [duckdb.StatementType,duckdb.ExpectedResultType,duckdb.ExplainType]:
 capture('members_'+cls.__name__,lambda cls=cls:[(k,int(v),v.name,repr(v),str(v),v==int(v),isinstance(v,int))for k,v in cls.__members__.items()])
 for x in [-1,29,30,255,256,None,1.5,True]:
  capture('enum_'+cls.__name__+'_'+repr(x),lambda cls=cls,x=x:[cls(x).name,cls(x).value,str(cls(x)),repr(cls(x)),int(cls(x)),hash(cls(x))])
 capture('pickle_'+cls.__name__,lambda cls=cls:repr(pickle.loads(pickle.dumps(cls(1)))))
for value in [0,1,2,255,-1,256,0.0,True,None,'','standard','analyze','STANDARD','bad',duckdb.ExplainType(2),duckdb.StatementType.INVALID]:
 capture('explain_eq_'+repr(value),lambda value=value:[duckdb.ExplainType.STANDARD==value,duckdb.ExplainType.ANALYZE!=value])
 def explain_argument(value=value):
  output=duckdb.sql('select 42').explain(value);return ['Query Profiling Information' in output, '42' in output]
 capture('explain_arg_'+repr(value),explain_argument)
for profiling in [False,True]:
 def explain_execution(profiling=profiling):
  c=duckdb.connect();seen=[]
  def bump(x):seen.append(x);return x
  c.create_function('explain_bump',bump,[duckdb.sqltypes.BIGINT],duckdb.sqltypes.BIGINT,side_effects=True)
  if profiling:c.execute('PRAGMA enable_profiling=no_output')
  output=c.sql('select explain_bump(i) x from range(3)t(i)').explain('analyze')
  return [seen,'Query profiling is disabled.' in output,'Query Profiling Information' in output,c.execute('select 7').fetchall()]
 capture('explain_exec_profile_'+str(profiling),explain_execution)
for args in [(),('select 1',)]:capture('constructor_'+repr(args),lambda args=args:duckdb.Statement(*args))
for method in ['execute','sql','query','from_query']:
 for parameterized in [False,True]:
  def run(method=method,parameterized=parameterized):
   a=duckdb.connect();b=duckdb.connect();stmt=a.extract_statements('select $1 + 1 answer'if parameterized else'select 42 answer')[0];a.close();kw=({'parameters':[42]}if method=='execute'else{'params':[42]})if parameterized else{};r=getattr(b,method)(stmt,**kw);return [r.fetchall(),stmt.query,sorted(stmt.named_parameters)]
  capture('run_'+method+'_'+str(parameterized),run)
 for value in [None,42,b'select 1',['select 1']]:
  capture('invalid_'+method+'_'+repr(value),lambda method=method,value=value:getattr(duckdb.connect(),method)(value))
for sql in ['select $1;select * from missing','select 1; select $1','select 1; select 42 as x']:
 def run(sql=sql):
  c=duckdb.connect();s=c.extract_statements(sql)[-1];return c.sql(s).fetchall()
 capture('later_statement_'+sql,run)
for method in ['execute','executemany','sql']:
 for sql in ['','--comment',';']:
  def run(method=method,sql=sql):
   c=duckdb.connect();c.execute('select 42');r=getattr(c,method)(sql);return [r is None,c.fetchall()]
  capture('empty_'+method+'_'+sql,run)
for outer in [None,[],(),42,'bad',{'x':1}]:
 def run(outer=outer):
  c=duckdb.connect();s=c.extract_statements('select $1')[0];r=c.executemany(s,outer);return [r is c,c.fetchall()]
 capture('many_outer_'+repr(outer),run)
def many():
 c=duckdb.connect();s=c.extract_statements('create table t(i int);insert into t values($1)');c.execute(s[0]);c.executemany(s[1],[(1,),(2,),(3,)]);result=c.fetchall();return [result,c.sql('from t').fetchall()]
capture('many_preparsed_insert',many)
def many_multi():
 c=duckdb.connect();c.executemany('create table t(i int);insert into t values($1)',[(1,),(2,)]);return [c.fetchall(),c.sql('from t').fetchall()]
capture('many_multi_once',many_multi)
def many_generator_failure():
 c=duckdb.connect();c.execute('create table t(i int)')
 def values():yield(1,);raise ValueError('late generator')
 try:c.executemany('insert into t values($1)',values())
 except Exception as e:return [type(e).__name__,str(e),c.sql('from t').fetchall()]
capture('many_generator_failure',many_generator_failure)
for qualified in [False,True]:
 for sql in ['','select 1','from missing','from "s.雪"."t.1"','from a join b using(i)','with c as(from a)from c','select(select count(*)from b)from a','from range(3)','pragma version','select 1;select 2','select * from a where i>1','insert into t select * from source']:
  capture('names_'+str(qualified)+'_'+sql,lambda sql=sql,qualified=qualified:sorted(duckdb.connect().get_table_names(sql,qualified=qualified)))
def stream_metadata():
 c=duckdb.connect();r=c.sql('select i from range(10001)t(i)').to_arrow_reader(1024);first=r.read_next_batch().column(0).to_pylist();names=c.get_table_names('pragma version');c.load_extension('core_functions');tail=r.read_all().column(0).to_pylist();return [first[0],len(first)+len(tail),sum(first)+sum(tail),sorted(names)]
capture('stream_metadata',stream_metadata)
for method,kwargs in [('install_extension',{'repository':'','version':'v'}),('install_extension',{'repository':'core','repository_url':'x'}),('install_extension',{'version':''}),('install_extension',{'repository':'unknown-repo'}),('install_extension',{'force_install':None,'repository':''}),('install_extension',{'force_install':1,'repository':''}),('load_extension',{})]:
 def run(method=method,kwargs=kwargs):
  c=duckdb.connect();return getattr(c,method)('core_functions',**kwargs)
 capture('extension_'+method+'_'+repr(kwargs),run)
def denied_load():
 c=duckdb.connect(config={'enable_external_access':False});c.load_extension('/not-present/native-extension.duckdb_extension')
capture('denied_load',denied_load)
pathlib.Path(sys.argv[1]).write_text(json.dumps({'outcomes':outcomes,'raw_errors':raw},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
