"""Large logical fsspec source: prefix reads must not materialize the entire file."""
import datetime,io,json,pathlib,resource,sys,time
import duckdb,fsspec
rows=int(sys.argv[1]);output=pathlib.Path(sys.argv[2]);stats={'read_bytes':0,'max_read_request':0,'opens':0}
class Handle(io.RawIOBase):
 def __init__(self):self.position=0
 def readable(self):return True
 def seekable(self):return True
 def tell(self):return self.position
 def seek(self,offset,whence=0):self.position=offset if whence==0 else self.position+offset if whence==1 else 2+rows*2+offset;return self.position
 def read(self,n=-1):
  if n<0 or n>64*1024*1024:raise RuntimeError('Whole-file materialization forbidden in this streaming fixture')
  stats['max_read_request']=max(stats['max_read_request'],n);n=min(n,max(0,2+rows*2-self.position));start=self.position;self.position+=n;stats['read_bytes']+=n
  if start<2:return b'i\n'[start:start+n]+(b'1\n'*((max(0,n-(2-start))+1)//2))[:max(0,n-(2-start))]
  return (b'1\n'*((n+2)//2))[(start-2)%2:(start-2)%2+n]
class Synthetic(fsspec.AbstractFileSystem):
 protocol='streamproof'
 def _open(self,path,mode='rb',**kwargs):stats['opens']+=1;return Handle()
 def size(self,path):return 2+rows*2
 def modified(self,path):return datetime.datetime(2020,1,1,tzinfo=datetime.timezone.utc)
 def glob(self,path,**kwargs):return ['streamproof://rows.csv']
 def isfile(self,path):return True
fs=Synthetic(skip_instance_cache=True);c=duckdb.connect(config={'threads':1});c.register_filesystem(fs);c.execute('select 42').fetchall();before=resource.getrusage(resource.RUSAGE_SELF);start=time.perf_counter();result=c.sql("select * from read_csv('streamproof://rows.csv',sample_size=20480) limit 1").fetchall();elapsed=time.perf_counter()-start;after=resource.getrusage(resource.RUSAGE_SELF);assert result==[(1,)],result;assert stats['read_bytes']<64*1024*1024,(rows,stats)
report={'rows':rows,'logical_source_bytes':2+rows*2,'result':result,'elapsed_seconds':elapsed,'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,**stats,'conditions':'realregisteredfsspecsource;logicalbytesgeneratedonlyonread;no wholesnapshot;freshprocess/1engineThread;RustdevvsCpprelease;notfinalacceptance'};output.write_text(json.dumps(report,indent=2)+'\n');c.close()
