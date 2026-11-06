"""Identify the sole DuckDB native engine on the supported Apple/Linux hosts."""
import ctypes,os,sys
from pathlib import Path

def loaded_engine_images(native):
    native=Path(native).resolve();paths=[]
    if sys.platform=='darwin':
        system=ctypes.CDLL('/usr/lib/libSystem.B.dylib')
        system._dyld_image_count.restype=ctypes.c_uint32
        system._dyld_get_image_name.argtypes=[ctypes.c_uint32]
        system._dyld_get_image_name.restype=ctypes.c_char_p
        paths=[Path(os.fsdecode(system._dyld_get_image_name(i))).resolve()for i in range(system._dyld_image_count())]
    else:
        for line in Path('/proc/self/maps').read_text().splitlines():
            fields=line.split(None,5)
            if len(fields)==6 and fields[5].startswith('/'):
                paths.append(Path(fields[5].removesuffix(' (deleted)')).resolve())
    images={str(path)for path in paths if path.name.startswith('_duckdb')or path.name in ['libduckdb.so','libduckdb.dylib']}
    assert images=={str(native)},images
    return sorted(images)
