"""Record Arrow-backed Pandas string scan, ownership and re-execution contracts."""
from native_hashing import sha256_stream
import ctypes, gc, hashlib, importlib, json, os, sys, warnings, weakref
from pathlib import Path
import duckdb, pandas as pd, pyarrow as pa
engine, expected, profile, dest = sys.argv[1:]
native = importlib.import_module('duckdb._duckdb' if engine == 'rust' else '_duckdb')
path = Path(native.__file__).resolve()
with path.open('rb') as f:
    assert sha256_stream(f) == expected
if sys.platform == 'darwin':
    system = ctypes.CDLL('/usr/lib/libSystem.B.dylib')
    system._dyld_image_count.restype = ctypes.c_uint32
    system._dyld_get_image_name.argtypes = [ctypes.c_uint32]
    system._dyld_get_image_name.restype = ctypes.c_char_p
    images = set()
    for index in range(system._dyld_image_count()):
        image = Path(os.fsdecode(system._dyld_get_image_name(index))).resolve()
        if image.name.startswith('_duckdb') or image.name == 'libduckdb.dylib':
            images.add(str(image))
else:
    images = {str(Path(x.split(None, 5)[5].removesuffix(' (deleted)')).resolve()) for x in Path('/proc/self/maps').read_text().splitlines() if len(x.split(None, 5)) == 6 and (Path(x.split(None, 5)[5]).name.startswith('_duckdb') or Path(x.split(None, 5)[5]).name == 'libduckdb.so')}
assert images == {str(path)}, images

def relation(con, frame, route):
    assert route == 'pandas'
    return con.from_df(frame)

def strings(v):
    return pd.Series(v, dtype='string[pyarrow]')

def frame():
    return pd.DataFrame({'x': strings(['duck', '雪', None, 'quack'])})

def cases():
    yield ('basic', frame(), None)
    yield ('unicode-nul-long', pd.DataFrame({'x': strings(['a\x00b', '🌍', 'é' * 4096, None, ''])}), None)
    yield ('empty', pd.DataFrame({'x': strings([])}), None)
    yield ('all-null', pd.DataFrame({'x': strings([None, None])}), None)
    f = frame()
    f.index = pd.Index([10, 11, 12, 13], name='named_index')
    yield ('named-index', f, None)
    f = frame()
    f.index = pd.MultiIndex.from_tuples([(1, 'a'), (1, 'b'), (2, 'a'), (2, 'b')], names=['first', 'second'])
    yield ('multi-index', f, None)
    f = pd.concat([strings(['a', None]), strings(['b', 'c']), strings(['d', 'e'])], axis=1)
    f.columns = ['X', 'x', 'x_1']
    yield ('duplicate-names', f, None)
    f = pd.concat([strings(['a']), strings(['b']), strings(['c'])], axis=1)
    f.columns = [1, ('a', 'b'), '雪']
    yield ('nonstring-column-names', f, None)
    arr = pd.arrays.ArrowStringArray(pa.chunked_array([pa.array(['a', None]), pa.array(['雪', ''])]))
    yield ('chunked', pd.DataFrame({'x': pd.Series(arr)}), None)
    yield ('mixed-object', pd.DataFrame({'x': strings(['a', None]), 'y': [{'a': 1}, {'a': 2}]}), None)
    yield ('mixed-integer', pd.DataFrame({'x': strings(['a', None]), 'y': [1, 2]}), None)
    yield ('mixed-python-string', pd.DataFrame({'x': strings(['a', None]), 'y': pd.Series(['b', 'c'], dtype='string[python]')}), None)
    yield ('replace-column', frame(), lambda f: f.__setitem__('x', strings(['NEW'] * 4)))
    yield ('mutate-element', frame(), lambda f: f.loc.__setitem__((0, 'x'), 'NEW'))
    yield ('rename-columns', frame(), lambda f: setattr(f, 'columns', ['renamed']))
    yield ('mutate-null', frame(), lambda f: f.loc.__setitem__((1, 'x'), None))
    yield ('append-row', frame(), lambda f: f.loc.__setitem__((4, 'x'), 'NEW'))
    yield ('owner-drop', frame(), 'drop')
_base_cases = cases

def cases():
    yield from _base_cases()
    values = ['duck', '雪', None, 'a\x00b'] * 1537
    chunks = [pa.array(values[:100]), pa.array(values[100:1948]), pa.array(values[1948:1957]), pa.array(values[1957:])]
    yield ('vector-and-arrow-chunk-boundaries', pd.DataFrame({'x': pd.Series(pd.arrays.ArrowStringArray(pa.chunked_array(chunks)))}), None)
    sliced = pa.array(['prefix'] * 9 + values).slice(13, 4097)
    yield ('nonzero-arrow-offset', pd.DataFrame({'x': pd.Series(pd.arrays.ArrowStringArray(pa.chunked_array([sliced])))}), None)
    nulls = pa.chunked_array([pa.array([None] * 9, type=pa.string()), pa.array([None] * 2050, type=pa.string())])
    yield ('all-null-multichunk', pd.DataFrame({'x': pd.Series(pd.arrays.ArrowStringArray(nulls))}), None)
    chunks = pa.chunked_array([pa.array([], type=pa.string()), pa.array(['a', None, '雪']), pa.array([], type=pa.string())])
    yield ('empty-arrow-chunks', pd.DataFrame({'x': pd.Series(pd.arrays.ArrowStringArray(chunks))}), None)
    yield ('mixed-nullable-integer', pd.DataFrame({'x': strings(['a', None]), 'y': pd.Series([1, None], dtype='Int64')}), None)
records = []
for route in ['pandas']:
    for name, f, mutate in cases():
        with duckdb.connect(config={'threads': 1}) as con:
            try:
                rel = relation(con, f, route)
                if mutate == 'drop':
                    ref = weakref.ref(f)
                    del f
                    gc.collect()
                elif mutate:
                    mutate(f)
                before = {'columns': rel.columns, 'types': [str(x) for x in rel.types], 'rows': rel.fetchall()}
                if name in ['mutate-element', 'mutate-null']:
                    f.loc[3, 'x'] = 'SECOND'
                    before['second_rows'] = rel.fetchall()
                records.append({'route': route, 'case': name, 'result': before})
            except Exception as e:
                records.append({'route': route, 'case': name, 'error': [type(e).__name__, str(e)]})
with path.open('rb') as source:
    assert sha256_stream(source) == expected
Path(dest).write_text(json.dumps({'engine': engine, 'arrow_profile': profile, 'pandas_version': pd.__version__, 'pyarrow_version': pa.__version__, 'native_sha256': expected, 'records': records}, indent=2, ensure_ascii=False) + '\n')
print(json.dumps({'engine': engine, 'profile': profile, 'cases': len(records)}, ensure_ascii=False), flush=True)
