"""Verify an installed package is the native Rust engine before running acceptance tests."""
import ctypes,hashlib,json,platform,sys
from pathlib import Path
import duckdb
from duckdb import _duckdb as native
assert native.__rust_engine__ is True
path=Path(native.__file__).resolve();assert path.suffix in ['.so','.pyd'],path
images=[]
if sys.platform=='darwin':
 library=ctypes.CDLL('/usr/lib/libSystem.B.dylib');library._dyld_image_count.restype=ctypes.c_uint32;library._dyld_get_image_name.argtypes=[ctypes.c_uint32];library._dyld_get_image_name.restype=ctypes.c_char_p
 images=[library._dyld_get_image_name(i).decode() for i in range(library._dyld_image_count())]
 engines=[name for name in images if Path(name).name.startswith('_duckdb') or Path(name).name=='libduckdb.dylib'];assert len(engines)==1 and Path(engines[0]).resolve()==path,engines
print(json.dumps({'python':sys.version,'architecture':platform.machine(),'package':duckdb.__file__,'extension':str(path),'extension_sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'version':duckdb.__version__,'engine_images':[name for name in images if 'duckdb' in Path(name).name]},indent=2))
