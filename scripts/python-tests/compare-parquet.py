"""Parquet native reads/writes, schemas, options and snapshot/query ownership."""
from pathlib import Path
from io import BytesIO
import gc,json,re,sys,weakref
import duckdb
import pyarrow as pa
import pyarrow.parquet as pq
out=Path(sys.argv[1]);root=out.parent;work=root/(out.stem+'-work');work.mkdir(exist_ok=False)
shared=root/'fixtures';shared.mkdir(exist_ok=True)
path=shared/'basic.parquet'
if not path.exists():pq.write_table(pa.table({'i':pa.array([1,2,3],type=pa.int32()),'s':['a',None,'雪'],'v':[[1,2],[],None]}),path)
other=shared/'other.parquet'
if not other.exists():pq.write_table(pa.table({'i':pa.array([4],type=pa.int32()),'s':['b'],'v':[[3]]}),other)
outcomes={};raw_errors={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as error:
  message=str(error);raw_errors[name]={'error':type(error).__name__,'message':message}
  message=re.sub(r'DUCKDB_INTERNAL_OBJECTSTORE://[a-f0-9]+','DUCKDB_INTERNAL_OBJECTSTORE://<generated>',message)
  message=message.replace(str(work),'<work>')
  if type(error).__name__=='InternalException':message=message.split('\nStack Trace:\n')[0].rstrip()
  message=re.sub(r'(_duckdb.DuckDBPyConnection object at )0x[0-9a-f]+',r'\1<address>',message)
  outcomes[name]={'error':type(error).__name__,'message':message}
def read(source=None,**kwargs):
 con=duckdb.connect(config={'threads':1});rel=con.read_parquet(str(path)if source is None else source,**kwargs)
 return [rel.type,rel.columns,[str(t)for t in rel.types],rel.fetchall()]
for name,source,kwargs in [
 ('basic',None,{}),('pathlib',path,{}),('bytes_path',bytes(path),{}),
 ('multiple',[str(path),str(other)],{}),('glob',str(shared/'*.parquet'),{}),
 ('filename',None,{'filename':True}),('row_number',None,{'file_row_number':True}),
 ('binary_as_string',None,{'binary_as_string':True}),('compression',None,{'compression':'snappy'}),
 ('bad_compression',None,{'compression':42}),('int_bool',None,{'filename':1}),
 ('none_bool',None,{'filename':None}),('str_bool',None,{'filename':'x'}),
 ('bytesio',BytesIO(path.read_bytes()),{}),('empty_list',[],{}),('nonfile',5,{}),
 ('bad_file',BytesIO(b'not a parquet file'),{}),
]:capture(name,lambda source=source,kwargs=kwargs:read(source,**kwargs))
def snapshot():
 con=duckdb.connect();source=BytesIO(path.read_bytes());ref=weakref.ref(source);rel=con.read_parquet(source);pos=source.tell();source.seek(0);source.write(b'bad!');del source;gc.collect();return [pos,ref()is None,rel.fetchall()]
capture('snapshot',snapshot)
def view_lifetime():
 con=duckdb.connect();rel=con.read_parquet(BytesIO(path.read_bytes()));rel.create_view('v');del rel;gc.collect();return con.sql('select * from v').fetchall()
capture('view_lifetime',view_lifetime)
def derived():
 con=duckdb.connect();rel=con.read_parquet(BytesIO(path.read_bytes())).filter('i>1');gc.collect();return rel.fetchall()
capture('derived_owner',derived)
def metadata_stream():
 con=duckdb.connect();con.execute('select i from range(5000)t(i)');a=con.fetchone();rel=con.read_parquet(str(path));b=con.fetchone();return[a,b,rel.fetchall()]
capture('metadata_stream',metadata_stream)
def sql_alias():
 con=duckdb.connect();rel=con.read_parquet(str(path));return [bool(re.fullmatch('parquet_[a-f0-9]{16}',rel.alias)),con.sql(rel.sql_query()).fetchall()]
capture('sql_alias',sql_alias)
def write(options):
 target=work/('result-'+str(len(list(work.iterdir())))+'.parquet');con=duckdb.connect(config={'threads':1});con.execute('set preserve_insertion_order=false')
 rel=con.sql("select i,case when i%3=0 then NULL else '雪'||i::varchar end s, [i, NULL] v from range(15)t(i)")
 result=rel.write_parquet(str(target),**options)
 return [result,con.read_parquet(str(target)).order('i').fetchall(),str(pq.read_schema(target))]
for name,options in {'default':{},'gzip':{'compression':'gzip'},'zstd':{'compression':'zstd'},'row_group':{'row_group_size':2048},'bytes_group':{'row_group_size_bytes':'2MB'},'field_auto':{'field_ids':'auto'},'field_nested':{'field_ids':{'i':1,'s':2,'v':{'__duckdb_field_id':3,'element':4}}},'bad_compression':{'compression':4},'bad_group':{'row_group_size':'a'},'bad_field':{'field_ids':3},'bad_nested':{'field_ids':{'i':'a'}},'bad_key':{'field_ids':{1:3}},'bad_partition':{'partition_by':'i'},'bad_append':{'append':1}}.items():capture('write_'+name,lambda options=options:write(options))
def access(filelike):
 con=duckdb.connect(config={'enable_external_access':False});return con.read_parquet(BytesIO(path.read_bytes())if filelike else str(path)).fetchall()
for filelike in [False,True]:capture('access_'+str(filelike),lambda filelike=filelike:access(filelike))
def argument_call(args,kwargs):
 con=duckdb.connect();return con.read_parquet(*args,**kwargs).fetchall()
for name,args,kwargs in [('missing',(),{}),('unknown',(str(path),),{'unknown':1}),('extra',(str(path),False,True),{}),('duplicate',(str(path),False),{'binary_as_string':True})]:capture('arguments_'+name,lambda args=args,kwargs=kwargs:argument_call(args,kwargs))

def copy_size(value):
 target=work/('sized-'+str(len(list(work.iterdir()))));con=duckdb.connect();con.sql('select i from range(10)t(i)').to_parquet(str(target),file_size_bytes=value);return con.read_parquet(str(target/'*.parquet')).fetchall()
for value in ['1k','1G','1MiB','-1','none','1e6B','1000','1foo']:
 capture('copy_size_'+value,lambda value=value:copy_size(value))

out.write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'raw_errors':raw_errors,'normalization':'only generated object-store identifiers, connection repr addresses, process output work directories and language-specific internal-error stack traces; raw errors retained'},indent=2,default=str)+'\n');print(json.dumps({'outcomes':len(outcomes)}))
