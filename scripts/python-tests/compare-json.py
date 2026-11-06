"""Frozen JSON reader options, native file scanning and ownership journeys."""
import gc,gzip,json,re,sys,weakref,os
from pathlib import Path
from io import BytesIO,StringIO
from collections import UserDict
import duckdb
output=Path(sys.argv[1]);root=Path(os.environ.get('JSON_SHARED_ROOT',output.parent));work=output.parent/(output.stem+'-work');work.mkdir();shared=root/'fixtures';shared.mkdir(exist_ok=True)
path=shared/'basic.json';text='{"i":1,"s":"雪","v":[1,2]}\n{"i":2,"s":null,"v":[]}\n';path.write_text(text)
other=shared/'other.json';other.write_text('{"i":3,"s":"b","extra":true}\n')
array=shared/'array.json';array.write_text('[{"i":1},{"i":2}]')
values=shared/'values.json';values.write_text('[1,2,3]')
bad=shared/'bad.json';bad.write_text('{"i":1}\nnot_json\n{"i":2}\n')
zipped=shared/'basic.json.gz';zipped.write_bytes(gzip.compress(text.encode()))
dated=shared/'dated.json';dated.write_text('{"d":"31/12/2020","ts":"31/12/2020 12:34:56","n":"42"}\n')
partition=shared/'part=7';partition.mkdir(exist_ok=True);partition_file=partition/'data.json';partition_file.write_text('{"i":42}\n')
outcomes={};raw_errors={}
def clean(v):
 if isinstance(v,(list,tuple)):return [clean(x)for x in v]
 if isinstance(v,dict):return {str(k):clean(x)for k,x in v.items()}
 if hasattr(v,'isoformat'):return v.isoformat()
 return v

def capture(name,action):
 if os.environ.get('JSON_LIST_ONLY'):outcomes[name]=None;return
 if os.environ.get('JSON_CASE') and name!=os.environ['JSON_CASE']:return
 print('case:',name,flush=True)
 try:outcomes[name]={'value':clean(action())}
 except Exception as e:
  message=str(e);raw_errors[name]={'error':type(e).__name__,'message':message}
  message=re.sub(r'DUCKDB_INTERNAL_OBJECTSTORE://[a-f0-9]+','DUCKDB_INTERNAL_OBJECTSTORE://<generated>',message)
  message=re.sub(r'(_duckdb.DuckDBPyConnection object at )0x[0-9a-f]+',r'\1<address>',message)
  if type(e).__name__=='SystemError':message=re.sub(r'^<built-in method read_json of .*?> returned', '<built-in method read_json of <binding>> returned',message)
  if type(e).__name__=='InternalException':message=message.split('\nStack Trace:\n')[0].rstrip()
  if type(e).__name__=='Error':message=message.split('\n\nAt:\n')[0].rstrip()
  outcomes[name]={'error':type(e).__name__,'message':message}
  if e.__cause__ is not None:outcomes[name]['cause']={'error':type(e.__cause__).__name__,'message':str(e.__cause__)}
 if os.environ.get('JSON_CASE'):
  output.write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'raw_errors':raw_errors},indent=2,ensure_ascii=False)+'\n');raise SystemExit(0)

def read(source=None,**kwargs):
 con=duckdb.connect(config={'threads':1});rel=con.read_json(str(path)if source is None else source,**kwargs)
 return [rel.type,rel.columns,[str(t)for t in rel.types],rel.fetchall()]
for name,source,options in [
 ('basic',None,{}),('pathlib',path,{}),('path_bytes',bytes(path),{}),('paths',[str(path),str(path)],{}),
 ('glob',str(shared/'basic.json'),{}),('array',str(array),{}),('values',str(values),{'records':'false'}),
 ('bytesio',BytesIO(text.encode()),{}),('stringio',StringIO(text),{}),('gzip',str(zipped),{}),
 ('gzip_bytesio',BytesIO(zipped.read_bytes()),{'compression':'gzip'}),
 ('explicit',None,{'columns':{'i':'INTEGER','s':'VARCHAR','v':'INTEGER[]'}}),('mapping',None,{'columns':UserDict({'i':'BIGINT'})}),
 ('union',[str(path),str(other)],{'union_by_name':True}),('filename',None,{'filename':True}),('filename_named',None,{'filename':'source'}),
 ('sample',None,{'sample_size':1}),('max_depth',None,{'maximum_depth':1}),('field_threshold',None,{'field_appearance_threshold':0.1}),
 ('map_threshold',None,{'map_inference_threshold':1}),('max_sample_files',None,{'maximum_sample_files':1}),
 ('ignored_errors',str(bad),{'format':'newline_delimited','ignore_errors':True}),
 ('empty_paths',[],{}),('tuple_path',(str(path),),{}),('nonfile',42,{}),('empty_buffer',BytesIO(b''),{}),('bad_buffer',BytesIO(b'not_json'),{}),
]:capture(name,lambda source=source,options=options:read(source,**options))
for name,source,options in [
 ('date_format',str(dated),{'columns':{'d':'DATE'},'date_format':'%d/%m/%Y'}),
 ('timestamp_format',str(dated),{'columns':{'ts':'TIMESTAMP'},'timestamp_format':'%d/%m/%Y %H:%M:%S'}),
 ('strings_integer',str(dated),{'convert_strings_to_integers':True}),
 ('strings_preserved',str(dated),{'convert_strings_to_integers':False}),
 ('hive',str(partition_file),{'hive_partitioning':True}),
 ('hive_types',str(partition_file),{'hive_partitioning':True,'hive_types':{'part':'VARCHAR'},'hive_types_autocast':False}),
 ('format_array',str(array),{'format':'array'}),('records_true',None,{'records':'true'}),
 ('maximum_object',None,{'maximum_object_size':1048576}),
]:capture(name,lambda source=source,options=options:read(source,**options))

for name,value in [('columns',[]),('columns',{1:'INTEGER'}),('columns',{'i':int}),('columns',{}),('columns',{'i':'unknown_type_name'}),('records',42),('format',42),('records','bad'),('format','bad'),('date_format',42),('timestamp_format',42),('compression',42),('compression','bad'),('sample_size',1.0),('sample_size',True),('sample_size',-1),('sample_size',2**40),('maximum_depth',0),('maximum_depth',-1),('maximum_depth',1.5),('maximum_object_size',-1),('maximum_object_size',2**40),('maximum_object_size','3'),('ignore_errors',1),('convert_strings_to_integers',1),('field_appearance_threshold',1),('field_appearance_threshold',-1.0),('map_inference_threshold',1.5),('map_inference_threshold',-1),('maximum_sample_files',1.5),('filename',42),('hive_partitioning',1),('union_by_name',1),('hive_types_autocast',1)]:
 capture('option_'+name+'_'+repr(value),lambda name=name,value=value:read(**{name:value}))

for key in ['sample_size','maximum_depth']:
 for value in [2**31,2**32-1,2**32,2**40+1,2**63,2**100,-2**100]:
  capture('integer_boundary_'+key+'_'+str(value),lambda key=key,value=value:read(**{key:value}))

for name,source,opts in [
 ('overflow_then_depth',None,{'sample_size':2**100,'maximum_depth':1.5}),
 ('overflow_then_map',None,{'sample_size':2**100,'map_inference_threshold':'x'}),
 ('overflow_bad_records',None,{'sample_size':2**100,'records':'bad'}),
 ('overflow_bad_file',BytesIO(b'not_json'),{'sample_size':2**100}),
 ('overflow_missing',str(shared/'missing.json'),{'sample_size':2**100}),
 ('overflow_memory',BytesIO(text.encode()),{'sample_size':2**100}),
 ('overflow_memory_records',BytesIO(text.encode()),{'sample_size':2**100,'records':'bad'}),
 ('overflow_memory_option',BytesIO(text.encode()),{'sample_size':2**100,'maximum_depth':1.5}),
]:capture(name,lambda source=source,opts=opts:read(source,**opts))

def snapshot():
 con=duckdb.connect();source=BytesIO(text.encode());ref=weakref.ref(source);rel=con.read_json(source);pos=source.tell();source.seek(0);source.write(b'bad!');del source;gc.collect();return [pos,ref()is None,rel.fetchall()]
capture('snapshot',snapshot)
def offset():
 source=BytesIO(b'garbage'+text.encode());source.seek(7);return read(source)
capture('offset',offset)
def view():
 con=duckdb.connect();rel=con.read_json(BytesIO(text.encode()));rel.create_view('v');del rel;gc.collect();return con.sql('select * from v').fetchall()
capture('view_owner',view)
def derived():
 con=duckdb.connect();rel=con.read_json(BytesIO(text.encode())).filter('i>1');gc.collect();return rel.fetchall()
capture('derived_owner',derived)
def stream():
 con=duckdb.connect();con.execute('select i from range(5000)t(i)');first=con.fetchone();rel=con.read_json(str(path));second=con.fetchone();return [first,second,rel.fetchall()]
capture('metadata_preserves_stream',stream)
def transaction():
 con=duckdb.connect();con.execute('create table t(i int)');con.execute('begin');con.execute('insert into t values(42)');con.read_json(str(path)).fetchall();con.execute('rollback');return con.sql('select * from t').fetchall()
capture('metadata_transaction',transaction)
def alias():
 con=duckdb.connect();rel=con.read_json(str(path));return [rel.alias,con.sql(rel.sql_query()).fetchall()]
capture('alias_sql',alias)
for filelike in [False,True]:
 def access(filelike=filelike):
  con=duckdb.connect(config={'enable_external_access':False});return con.read_json(BytesIO(text.encode())if filelike else str(path)).fetchall()
 capture('access_'+str(filelike),access)
for name,args,kwargs in [('missing',(),{}),('unknown',(str(path),),{'unknown':1}),('extra',(str(path),{}),{}),('duplicate',(str(path),),{'path_or_buffer':str(path)}),('records_before_path',(None,),{'records':42}),('path_before_columns',(None,),{'columns':42})]:
 def call(args=args,kwargs=kwargs):return duckdb.connect().read_json(*args,**kwargs).fetchall()
 capture('args_'+name,call)
def closed():
 con=duckdb.connect();con.close();return con.read_json(None,columns=42)
capture('closed',closed)
capture('module',lambda:duckdb.read_json(str(path)).fetchall())
output.write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'raw_errors':raw_errors,'normalization':'generated object-store IDs, owner repr addresses, interpreter binding descriptor in SystemError, Python callback stack frames and native internal stack traces only; raw errors and exception causes retained'},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
