"""Check native object-array scans against literal rows and the frozen C++ boundary.

Usage: python object-buffer-boundaries.py {cpp,rust} OUTPUT.json
Run separately in each pinned environment, then compare the records. These controls
include expected negative-stride errors and a NumPy subclass buffer override; they
also record the actual signed native image on Apple and Linux test hosts.
"""
import pathlib,sys,json,ctypes,hashlib,gc
import numpy as np,pandas as pd,duckdb,_duckdb
engine,output=sys.argv[1:]
assert bool(getattr(_duckdb,'__rust_engine__',False))==(engine=='rust')
native=pathlib.Path(_duckdb.__file__).resolve()
records=[]
base=["duck","雪",None,"", "a\x00b", "שלום","x"*100, "🙂", pd.NA, float('nan'), "tail", "last"]
for storage in ['numpy','pandas-python','pandas-arrow']:
 for stride in [1,2,-1,-2]:
  a=np.array(base,dtype=object)[::stride]
  if storage=='numpy':frame=pd.DataFrame({'x':a},copy=False)
  else:frame=pd.DataFrame({'x':pd.Series(a,dtype='string['+('python'if storage=='pandas-python'else'pyarrow')+']')})
  expected=[None if v is None or v is pd.NA or isinstance(v,float)and np.isnan(v) else v for v in a]
  negative=frame['x'].to_numpy().strides[0]<0
  with duckdb.connect()as c:
   try:relation=c.from_df(frame)
   except Exception as error:
    assert negative and type(error).__name__=='Error' and str(error)=="Unable to cast Python instance of type <class 'int'> to C++ type '?' (#define PYBIND11_DETAILED_ERROR_MESSAGES or compile in debug mode for details)",(storage,stride,type(error).__name__,str(error))
    records.append({'storage':storage,'stride':stride,'expected_error':type(error).__name__,'message':str(error)})
   else:
    assert not negative
    got=relation.fetchall();assert got==[(v,)for v in expected],(storage,stride,got,expected);records.append({'storage':storage,'stride':stride,'rows':got})
# A zero-stride NumPy view and negative strides must retain row order and ownership.
for stride in [0,8,-8]:
 origin=np.array(['α','β','γ','δ'],dtype=object)
 if stride==0:a=np.lib.stride_tricks.as_strided(origin,shape=(20,),strides=(0,))
 elif stride>0:a=origin
 else:a=origin[::-1]
 with duckdb.connect()as c:
  try:c.register('incoming',pd.DataFrame({'x':a},copy=False))
  except Exception as error:
   assert stride<0 and type(error).__name__=='Error' and "Unable to cast Python instance of type <class 'int'>" in str(error)
   records.append({'storage':'numpy-view','stride':stride,'expected_error':type(error).__name__,'message':str(error)})
  else:
   assert stride>=0
   got=c.sql('FROM incoming').fetchall();assert got==[(v,)for v in a];records.append({'storage':'numpy-view','stride':stride,'rows':got})
# Mutation from __str__ must affect later slots, without invalidating the current object.
a=np.array([None,'before','tail'],dtype=object)
class Change:
 def __str__(self):
  a[0]=None;a[1]='changed';return 'converted'
a[0]=Change()
with duckdb.connect()as c:
 c.register('incoming',pd.DataFrame({'x':a},copy=False));got=c.sql('FROM incoming').fetchall();assert got==[('converted',),('changed',),('tail',)],got;records.append({'storage':'reentrant-object','rows':got})
# A subclass buffer override must not change the existing indexed scan values.
calls=[];replacement=np.array(['replaced','values'],dtype=object)
class Custom(np.ndarray):
 def __buffer__(self,flags):
  calls.append('buffer');return memoryview(replacement)
a=np.array(['original','values'],dtype=object).view(Custom)
with duckdb.connect()as c:
 c.register('incoming',{'x':a});got=c.sql('FROM incoming').fetchall();assert got==[('original',),('values',)] and calls==[],(got,calls);records.append({'storage':'custom-buffer-subclass','rows':got,'calls':calls})
gc.collect()
if sys.platform == 'darwin':
 system=ctypes.CDLL('/usr/lib/libSystem.B.dylib')
 system._dyld_image_count.restype=ctypes.c_uint32
 system._dyld_get_image_name.argtypes=[ctypes.c_uint32]
 system._dyld_get_image_name.restype=ctypes.c_char_p
 images=[system._dyld_get_image_name(i).decode()for i in range(system._dyld_image_count())]
 images=[n for n in images if pathlib.Path(n).name.startswith('_duckdb')or pathlib.Path(n).name=='libduckdb.dylib']
elif sys.platform.startswith('linux'):
 mapped=set()
 for line in pathlib.Path('/proc/self/maps').read_text().splitlines():
  fields=line.split(None,5)
  if len(fields)!=6 or not fields[5].startswith('/'):
   continue
  image=pathlib.Path(fields[5]).resolve()
  if image.name.startswith('_duckdb')or image.name=='libduckdb.so':
   mapped.add(str(image))
 images=sorted(mapped)
else:
 raise RuntimeError('Loaded-image observation is unsupported on '+sys.platform)
assert len(images)==1 and pathlib.Path(images[0]).resolve()==native
pathlib.Path(output).write_text(json.dumps({'passed':True,'records':records,'native':str(native),'native_sha256':hashlib.sha256(native.read_bytes()).hexdigest(),'engine_images':images},indent=2)+'\n');print(engine,len(records),'object-buffer controls PASS')
