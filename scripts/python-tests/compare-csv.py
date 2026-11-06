"""CSV bytes, path/file ownership, option validation and retained native query inputs."""
from pathlib import Path
from io import BytesIO,StringIO
import gc,json,sys,weakref,re
import duckdb
out=Path(sys.argv[1]);root=out.parent;work=root/(out.stem+'-work');work.mkdir(exist_ok=False)
shared=root/'fixtures';shared.mkdir(exist_ok=True)
fixtures={'basic.csv':'a,b,s\n1,2,hello\n3,,雪\n','other.csv':'a,b,s\n4,5,world\n','semi.csv':'a;b\n1;2\n','dates.csv':'d,t\n2020-01-02,2020-01-02 03:04:05\n','types.csv':'a,b\n1,2\n3,4\n'}
for name,text in fixtures.items():
 p=shared/name
 if not p.exists():p.write_text(text)
outcomes={}
raw_errors={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as error:
  raw_errors[name]={'error':type(error).__name__,'message':str(error)}
  # Object-store paths are generated independently in each C++/Rust process.
  message=re.sub(r'DUCKDB_INTERNAL_OBJECTSTORE://[a-f0-9]+','DUCKDB_INTERNAL_OBJECTSTORE://<generated>',str(error))
  outcomes[name]={'error':type(error).__name__,'message':message}
def read(source=None,**kwargs):
 con=duckdb.connect(config={'threads':1});rel=con.read_csv(str(shared/'basic.csv') if source is None else source,**kwargs)
 return [rel.type,rel.columns,[str(t)for t in rel.types],rel.fetchall()]
for name,source,kwargs in [
 ('basic',None,{}),('pathlib',shared/'basic.csv',{}),('bytes',bytes(shared/'basic.csv'),{}),
 ('multiple',[str(shared/'basic.csv'),str(shared/'other.csv')],{}),('names',None,{'names':['x','y','z']}),
 ('dtype',None,{'dtype':{'a':str}}),('dtype_list',None,{'dtype':['varchar']}),
 ('all_varchar',None,{'all_varchar':True}),('no_header',str(shared/'types.csv'),{'header':False}),
 ('columns',str(shared/'types.csv'),{'columns':{'x':'int','y':'int'},'header':True,'auto_detect':False}),
 ('sep',str(shared/'semi.csv'),{'sep':';'}),('null',None,{'na_values':['hello','雪']}),
 ('stringio',StringIO('a,b\n1,雪\n'),{}),('bytesio',BytesIO(b'a,b\n1,2\n'),{}),
 ('empty_paths',[],{}),('non_file',5,{}),
 ('encoding',None,{'encoding':'latin1'}),('header_invalid',None,{'header':2}),
 ('dtype_invalid',None,{'dtype':5}),('names_invalid',None,{'names':[1]}),
 ('bad_key',None,{'delimeter':','}),('bad_unknown',None,{'foo':1}),
 ('lineterminator',None,{'lineterminator':'LINE_FEED'}),('enum_line',None,{'lineterminator':duckdb.CSVLineTerminator.LINE_FEED}),
 ('line_invalid',None,{'lineterminator':5}),('line_bad',None,{'lineterminator':'x'}),
 ('sep_conflict',None,{'sep':',','delimiter':';'}),
]:capture(name,lambda source=source,kwargs=kwargs:read(source,**kwargs))

def file_snapshot():
 con=duckdb.connect();source=BytesIO(b'a,b\n1,2\n');ref=weakref.ref(source);rel=con.read_csv(source)
 position=source.tell();source.seek(0);source.write(b'a,b\n3,4\n');del source;gc.collect()
 return [position,ref()is None,rel.fetchall(),rel.fetchall()]
capture('file_snapshot',file_snapshot)
def live_path():
 p=work/'live.csv';p.write_text('a,b\n1,2\n');con=duckdb.connect();rel=con.read_csv(str(p));p.write_text('a,b\n3,4\n');return rel.fetchall()
capture('live_path',live_path)
def metadata_stream():
 con=duckdb.connect();con.execute('select i from range(5000)t(i)');first=con.fetchone();rel=con.read_csv(str(shared/'basic.csv'));second=con.fetchone();return [first,second,rel.fetchall()]
capture('metadata_stream',metadata_stream)
def retained_view():
 con=duckdb.connect();source=BytesIO(b'a\n1\n2\n');rel=con.read_csv(source);rel.create_view('v');del rel,source;gc.collect();return con.sql('select sum(a) from v').fetchall()
capture('retained_view',retained_view)

def write(options):
 p=work/('write-'+str(len(list(work.iterdir())))+'.csv');con=duckdb.connect();rel=con.sql("select * from (values(1,'a,b'),(2,NULL),(3,'雪'))t(i,s)");result=rel.to_csv(str(p),**options)
 return [result,p.read_text(),con.read_csv(str(p),header=options.get('header',True),sep=options.get('sep',','),quotechar=options.get('quotechar','"'),escapechar=options.get('escapechar','"')).fetchall()]
for name,options in {'default':{},'sep':{'sep':';'},'header_false':{'header':False},'null':{'na_rep':'missing'},'quoted':{'quoting':'all'},'quote':{'quotechar':"'",'escapechar':'!'},'bad_header':{'header':1},'bad_encoding':{'encoding':'latin1'},'bad_quote':{'quoting':0},'bad_partition':{'partition_by':'i'}}.items():capture('write_'+name,lambda options=options:write(options))
def alias_sql():
 con=duckdb.connect();rel=con.read_csv(str(shared/'basic.csv'));query=rel.sql_query();rows=con.sql(query).fetchall()
 return [rel.alias,rows]
capture('alias_sql_roundtrip',alias_sql)
def derived_owner():
 con=duckdb.connect();rel=con.read_csv(BytesIO(b'a\n1\n2\n')).filter('a>1');gc.collect();return rel.fetchall()
capture('derived_owner',derived_owner)
def copied_compressed():
 import gzip
 return read(BytesIO(gzip.compress(b'a,b\n1,2\n')),compression='gzip')
capture('compressed_filelike',copied_compressed)
def frozen_schema():
 p=work/'types.csv';p.write_text('a,b\n1,2\n');con=duckdb.connect();rel=con.read_csv(str(p));p.write_text('a,b\n3,4\n');return [rel.columns,[str(t)for t in rel.types],rel.fetchall()]
capture('frozen_schema',frozen_schema)

def external_disabled(filelike):
 con=duckdb.connect(config={'enable_external_access':False});source=BytesIO(b'a\n1\n')
 try:con.read_csv(source if filelike else str(shared/'basic.csv'))
 except Exception as error:
  raw_errors['external_disabled_'+str(filelike)]={'error':type(error).__name__,'message':str(error)}
  return [type(error).__name__,re.sub(r'DUCKDB_INTERNAL_OBJECTSTORE://[a-f0-9]+','DUCKDB_INTERNAL_OBJECTSTORE://<generated>',str(error)),source.tell()]
 return 'unexpected success'
for filelike in [False,True]:capture('external_disabled_'+str(filelike),lambda filelike=filelike:external_disabled(filelike))
def disabled_filesystem(name):
 con=duckdb.connect();con.execute("set disabled_filesystems='"+name+"'");return con.read_csv(BytesIO(b'a\n1\n')).fetchall()
for name in ['LocalFileSystem','DUCKDB_INTERNAL_OBJECTSTORE']:capture('disabled_'+name,lambda name=name:disabled_filesystem(name))

def derived_view():
 con=duckdb.connect();rel=con.read_csv(BytesIO(b'a\n1\n2\n')).filter('a>1');rel.create_view('v');del rel;gc.collect();return con.sql('select * from v').fetchall()
capture('derived_view_lifetime',derived_view)
def query_lifetime():
 con=duckdb.connect();rel=con.read_csv(BytesIO(b'a\n1\n2\n')).query('x','select sum(a) from x');gc.collect();return rel.fetchall()
capture('query_lifetime',query_lifetime)

for name,kwargs in {'filename':{'filename':True},'named_filename':{'filename':'source_file'},'union_filename':{'filename':True,'union_by_name':True}}.items():capture(name,lambda kwargs=kwargs:read(**kwargs))
def filename_metadata():
 con=duckdb.connect();rel=con.read_csv(str(shared/'basic.csv'),filename=True);before=[rel.columns,[str(t)for t in rel.types]];rows=rel.fetchall();return [before,rows,rel.columns,[str(t)for t in rel.types]]
capture('filename_metadata',filename_metadata)

class CSVLineTerminator(int):pass
capture('foreign_enum_type',lambda:read(lineterminator=CSVLineTerminator(9)))

out.write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'raw_errors':raw_errors,'normalization':'only generated object-store URI identifiers; raw exception text retained'},indent=2,default=str)+'\n');print(json.dumps({'outcomes':len(outcomes)}))
