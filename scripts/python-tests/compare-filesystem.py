"""Actual registered-filesystem journeys, including streaming and ownership."""
import gc,json,pathlib,re,sys,weakref
import duckdb,fsspec
outcomes={};raw={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as e:
  raw[name]={'error':type(e).__name__,'message':str(e)};outcomes[name]={'error':type(e).__name__,'message':re.sub(r' at 0x[0-9a-f]+',r' at <address>',str(e))}
def filesystem():
 fs=fsspec.filesystem('memory',skip_instance_cache=True);fs.store={};fs.pseudo_dirs=[''];return fs
for format,extension in [('csv','csv'),('parquet','parquet'),('json','json')]:
 def run(format=format,extension=extension):
  c=duckdb.connect();fs=filesystem();c.register_filesystem(fs);path=f'memory://root/data.{extension}';c.execute(f"copy(select i,'雪' s from range(5)t(i))to'{path}'(format {format})");rows=c.sql(f"select * from read_{format}('{path}')order by i").fetchall();return [rows,len(fs.cat(path))>0,c.list_filesystems(),c.filesystem_is_registered('memory')]
 capture('roundtrip_'+format,run)
def share():
 c=duckdb.connect();child=c.cursor();fs=filesystem();c.register_filesystem(fs);before=child.list_filesystems();child.unregister_filesystem('memory');return [before,c.list_filesystems()]
capture('shared_database_registration',share)
def retained():
 c=duckdb.connect();fs=filesystem();fs.pipe('x.csv',b'i\n1\n2\n');ref=weakref.ref(fs);c.register_filesystem(fs);del fs;gc.collect();alive=ref()is not None;rows=c.sql("from read_csv('memory://x.csv')").fetchall();c.unregister_filesystem('memory');gc.collect();return [alive,rows,ref()is None]
capture('filesystem_ownership',retained)
def duplicate():
 c=duckdb.connect();fs=filesystem();c.register_filesystem(fs);c.register_filesystem(fs)
capture('duplicate_registration',duplicate)
for value in [None,42,'memory']:
 capture('invalid_'+repr(value),lambda value=value:duckdb.connect().register_filesystem(value))
for method in ['register_filesystem','unregister_filesystem','list_filesystems','filesystem_is_registered']:
 def run(method=method):
  c=duckdb.connect();c.close();args=(filesystem(),)if method=='register_filesystem'else('memory',)if method!='list_filesystems'else();return getattr(c,method)(*args)
 capture('closed_'+method,run)
def disabled():
 c=duckdb.connect(config={'enable_external_access':False});fs=filesystem();fs.pipe('x.csv',b'i\n1\n');c.register_filesystem(fs);return c.sql("from read_csv('memory://x.csv')").fetchall()
capture('external_access_disabled',disabled)
def overwritten():
 c=duckdb.connect();fs=filesystem();c.register_filesystem(fs);c.execute("copy(select 1 i)to'memory://x.csv'(format csv)");c.execute("copy(select 2 i)to'memory://x.csv'(format csv)");return [c.sql("from read_csv('memory://x.csv')").fetchall(),sorted(fs.find('/'))]
capture('overwrite_same_file',overwritten)
def compressed():
 c=duckdb.connect();fs=filesystem();c.register_filesystem(fs);c.execute("copy(select i from range(100)t(i))to'memory://x.csv.gz'(format csv,compression gzip)");return c.sql("select count(*),sum(i)from read_csv('memory://x.csv.gz')").fetchall()
capture('compressed_csv',compressed)
pathlib.Path(sys.argv[1]).write_text(json.dumps({'outcomes':outcomes,'raw_errors':raw},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
