"""Public Python named-memory lifetime, sharing and path-independence controls."""
from native_hashing import sha256_stream
import ctypes, gc, hashlib, importlib, json, os, sys
from pathlib import Path
import duckdb
engine, expected, directory, report_path = sys.argv[1:]
root = Path(directory)
root.mkdir()
os.chdir(root)
native = importlib.import_module('duckdb._duckdb' if engine == 'rust' else '_duckdb')
library = Path(native.__file__).resolve()

def identity():
    with library.open('rb') as f:
        assert sha256_stream(f) == expected
    images = set()
    if sys.platform == 'darwin':
        system = ctypes.CDLL('/usr/lib/libSystem.B.dylib')
        system._dyld_image_count.restype = ctypes.c_uint32
        system._dyld_get_image_name.argtypes = [ctypes.c_uint32]
        system._dyld_get_image_name.restype = ctypes.c_char_p
        for index in range(system._dyld_image_count()):
            p = Path(os.fsdecode(system._dyld_get_image_name(index))).resolve()
            if p.name.startswith('_duckdb') or p.name == 'libduckdb.dylib':
                images.add(str(p))
    else:
        for line in Path('/proc/self/maps').read_text().splitlines():
            parts = line.split(None, 5)
            if len(parts) == 6 and parts[5].startswith('/'):
                p = Path(parts[5].removesuffix(' (deleted)')).resolve()
                if p.name.startswith('_duckdb') or p.name == 'libduckdb.so':
                    images.add(str(p))
    assert images == {str(library)}, images
identity()
records = []

def count(con):
    return con.execute("SELECT count(*) FROM information_schema.tables WHERE table_name='marker'").fetchone()[0]

def close(*connections):
    for c in connections:
        c.close()
    gc.collect()
a = duckdb.connect(':memory:exact')
a.execute('CREATE TABLE marker AS SELECT 42 AS value')
b = duckdb.connect(':memory:exact')
records.append({'case': 'live-same-name', 'rows': b.execute('SELECT value FROM marker').fetchall()})
records.append({'case': 'introspection', 'database': b.execute('SELECT current_database()').fetchone()[0], 'databases': b.execute('SELECT path FROM duckdb_databases() WHERE NOT internal').fetchall()})
close(a, b)
del a, b
gc.collect()
c = duckdb.connect(':memory:exact')
records.append({'case': 'last-close-drops-data', 'count': count(c)})
close(c)
del c
a = duckdb.connect(':memory:cwd')
a.execute('CREATE TABLE marker AS SELECT 42 AS value')
other = root / 'other'
other.mkdir()
os.chdir(other)
b = duckdb.connect(':memory:cwd')
records.append({'case': 'cwd-independent', 'count': count(b)})
close(a, b)
del a, b
os.chdir(root)
a = duckdb.connect(':memory:Upper')
a.execute('CREATE TABLE marker AS SELECT 42 AS value')
b = duckdb.connect(':memory:upper')
records.append({'case': 'case-contract', 'count': count(b)})
close(a, b)
del a, b
a = duckdb.connect(':memory:one')
a.execute('CREATE TABLE marker AS SELECT 42 AS value')
b = duckdb.connect(':memory:two')
records.append({'case': 'different-names-independent', 'count': count(b)})
close(a, b)
del a, b
a = duckdb.connect()
a.execute('CREATE TABLE marker AS SELECT 42 AS value')
b = duckdb.connect()
records.append({'case': 'unnamed-independent', 'count': count(b)})
close(a, b)
del a, b
a = duckdb.connect(':memory:readonly')
try:
    b = duckdb.connect(':memory:readonly', read_only=True)
    close(b)
    error = None
except Exception as e:
    error = [type(e).__name__, str(e)]
records.append({'case': 'configuration-mismatch', 'error': error})
close(a)
del a
for label, first, second in [('dot-segment-is-name-data', ':memory:dir/./name', ':memory:dir/name'), ('repeated-separator-is-name-data', ':memory:other//name', ':memory:other/name')]:
    a = duckdb.connect(first)
    a.execute('CREATE TABLE marker AS SELECT 42 AS value')
    b = duckdb.connect(second)
    records.append({'case': label, 'count': count(b)})
    close(a, b)
    del a, b
gc.collect()
files = sorted((str(p.relative_to(root)) for p in root.rglob('*') if p.is_file()))
records.append({'case': 'no-persistent-files', 'files': files})
identity()
Path(report_path).write_text(json.dumps({'engine': engine, 'native_sha256': expected, 'records': records}, indent=2) + '\n')
print(json.dumps({'engine': engine, 'cases': len(records)}), flush=True)
