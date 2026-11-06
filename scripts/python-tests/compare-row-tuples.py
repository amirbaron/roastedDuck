"""Unnamed STRUCT values preserve every positional field through Python result converters."""
import json,sys
from pathlib import Path
import duckdb
outcomes={}
def clean(v):
 if isinstance(v,dict):return {str(k):clean(x)for k,x in v.items()}
 if isinstance(v,tuple):return {'tuple':[clean(x)for x in v]}
 if isinstance(v,list):return [clean(x)for x in v]
 if hasattr(v,'tolist'):return clean(v.tolist())
 return v

def capture(name,action):
 try:outcomes[name]={'value':clean(action())}
 except Exception as e:outcomes[name]={'error':type(e).__name__,'message':str(e)}
queries={'row':'select row(1,2,3) v','nullable':'select row(1,NULL,3) v','nested':"select {'a':row(1,2),'b':row('雪',NULL)} v",'zip':'select list_zip([1,2,3],[2,4,6],[3,6]) v','array':'select [row(1,2),row(3,4)] v','named':"select {'a':1,'b':2} v"}
for name,query in queries.items():
 for mode in ['fetchall','numpy','pandas']:
  def run(query=query,mode=mode):
   rel=duckdb.sql(query)
   if mode=='fetchall':return rel.fetchall()
   if mode=='numpy':return rel.fetchnumpy()['v'].tolist()
   return rel.df()['v'].tolist()
  capture(name+'_'+mode,run)
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'normalization':'NumPy scalar/array formatting only; tuple identity and all positional fields retained'},indent=2,ensure_ascii=False)+'\n')
