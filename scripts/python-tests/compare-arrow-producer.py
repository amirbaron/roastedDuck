"""Producer projection/filter correctness, retained owners and actual dataset pruning."""
import gc,json,pathlib,re,sys,weakref
import duckdb,pyarrow as pa,pyarrow.dataset as ds,pyarrow.parquet as pq
out=pathlib.Path(sys.argv[1]);fixtures=pathlib.Path(sys.argv[2]);fixtures.mkdir(exist_ok=True);outcomes={};raw={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as e:
  raw[name]={'error':type(e).__name__,'message':str(e)};outcomes[name]={'error':type(e).__name__,'message':re.sub(r'(_duckdb.DuckDBPy(?:Connection|Relation) object at )0x[0-9a-f]+',r'\1<address>',str(e))}
source=pa.table({'a':[1,2,3],'b':['a','b','c'],'c':[True,False,True]})
for factory in ['table','batch','reader','dataset','scanner','capsule']:
 def run(factory=factory):
  c=duckdb.connect();obj={'table':lambda:source,'batch':lambda:source.to_batches()[0],'reader':source.to_reader,'dataset':lambda:ds.dataset(source),'scanner':lambda:ds.dataset(source).scanner(columns=['b','a','c']),'capsule':source.__arrow_c_stream__}[factory]();r=c.from_arrow(obj);return [r.filter('c').project('b,a').fetchall(),r.columns,[str(t)for t in r.types]]
 capture('project_filter_'+factory,run)
for sql in ['select count(*) from obj','select b from obj where a>1','select a from obj where b=\'c\'','select c,b from obj where a in(1,3)','select a from obj where a=999','select b from obj where a>1 or a<0']:
 for kind in ['table','dataset']:
  def run(sql=sql,kind=kind):
   c=duckdb.connect();obj=source if kind=='table'else ds.dataset(source);return c.sql(sql).fetchall()
  capture(kind+'_'+sql,run)
class Owner:
 def __init__(self):self.table=source;self.schema=source.schema
 def __arrow_c_stream__(self,requested_schema=None):return self.table.__arrow_c_stream__(requested_schema=requested_schema)
def lifetime():
 c=duckdb.connect();obj=Owner();ref=weakref.ref(obj);r=c.from_arrow(obj).filter('a>1').project('b');del obj;gc.collect();alive=ref()is not None;reader=r.to_arrow_reader(1);del r;gc.collect();during=ref()is not None;rows=reader.read_all().to_pylist();reader.close();del reader;c.execute('select 42').fetchall();gc.collect();return [alive,during,rows,ref()is None]
capture('owner_lifetime',lifetime)
def prunes():
 root=fixtures/'partitioned';good=root/'part=good';bad=root/'part=bad';good.mkdir(parents=True,exist_ok=True);bad.mkdir(exist_ok=True);pq.write_table(pa.table({'i':[1,2,3]}),good/'data.parquet');(bad/'data.parquet').write_bytes(b'not a valid parquet file')
 dataset=ds.dataset(root,format='parquet',partitioning='hive',schema=pa.schema([('i',pa.int64()),('part',pa.string())]),exclude_invalid_files=False)
 c=duckdb.connect();return c.sql("select i from dataset where part='good' order by i").fetchall()
capture('actual_partition_pruning',prunes)
out.write_text(json.dumps({'outcomes':outcomes,'raw_errors':raw},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
