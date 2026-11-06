"""Installed ADBC facade: one native image and sequential Python/ADBC file handoff."""
import argparse
import ctypes
from decimal import Decimal
import json
from pathlib import Path

# Exercise driver discovery before importing duckdb or its top-level native shim.
import adbc_driver_duckdb
import adbc_driver_duckdb.dbapi

driver = Path(adbc_driver_duckdb.driver_path()).resolve()
import duckdb
import _duckdb as native

parser = argparse.ArgumentParser()
parser.add_argument('--work', type=Path, required=True)
parser.add_argument('--rust', action='store_true')
args = parser.parse_args()
assert bool(getattr(native, '__rust_engine__', False)) == args.rust
assert driver == Path(native.__file__).resolve()
library = ctypes.CDLL(str(driver))
library.duckdb_library_version.restype = ctypes.c_char_p
assert library.duckdb_library_version() == b'v1.5.6'
assert library.duckdb_adbc_init
args.work.mkdir(parents=True, exist_ok=False)
database = str(args.work / 'shared.duckdb')
con = duckdb.connect(database)
con.execute('CREATE TABLE shared(i INTEGER, s VARCHAR)')
con.execute("INSERT INTO shared VALUES (1, 'python')")
con.create_function('python_twice', lambda value: value * 2, ['BIGINT'], 'BIGINT')
con.execute("INSERT INTO shared SELECT python_twice(3), 'python udf'")
con.remove_function('python_twice')
con.close()
adbc = adbc_driver_duckdb.dbapi.connect(database)
outcomes = {}
with adbc.cursor() as cursor:
    cursor.execute('SELECT * FROM shared ORDER BY i')
    outcomes['python_to_adbc'] = cursor.fetchall()
    assert outcomes['python_to_adbc'] == [(1, 'python'), (6, 'python udf')]
    cursor.execute('INSERT INTO shared VALUES (?, ?)', [2, 'adbc'])
adbc.rollback()
with adbc.cursor() as cursor:
    cursor.execute('SELECT * FROM shared ORDER BY i')
    outcomes['rollback'] = cursor.fetchall()
    assert outcomes['rollback'] == [(1, 'python'), (6, 'python udf')]
    cursor.execute('INSERT INTO shared VALUES (?, ?)', [2, 'adbc'])
adbc.commit()
with adbc.cursor() as cursor:
    cursor.execute('SELECT sum(i) FROM shared')
    outcomes['committed'] = cursor.fetchall()
    assert outcomes['committed'] == [(9,)]
adbc.close()
with duckdb.connect(database) as reopened:
    outcomes['reopen'] = reopened.execute('SELECT * FROM shared ORDER BY i').fetchall()
    assert outcomes['reopen'] == [(1, 'python'), (2, 'adbc'), (6, 'python udf')]
system = ctypes.CDLL('/usr/lib/libSystem.B.dylib')
system._dyld_image_count.restype = ctypes.c_uint32
system._dyld_get_image_name.argtypes = [ctypes.c_uint32]
system._dyld_get_image_name.restype = ctypes.c_char_p
images = [system._dyld_get_image_name(i).decode() for i in range(system._dyld_image_count())]
engines = [name for name in images if Path(name).name.startswith('_duckdb') or Path(name).name == 'libduckdb.dylib']
assert len(engines) == 1 and Path(engines[0]).resolve() == driver, engines
def encode(value):
    if isinstance(value, Decimal):
        return {'python_type': 'Decimal', 'value': str(value)}
    raise TypeError(type(value).__name__)

print(json.dumps({'outcomes': outcomes, 'driver': str(driver), 'engine_images': engines, 'rust': args.rust}, default=encode))
