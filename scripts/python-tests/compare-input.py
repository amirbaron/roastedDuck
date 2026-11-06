"""Native Arrow input semantics and ownership, compared with frozen C++ in isolation."""
import datetime
import decimal
import gc
import json
from pathlib import Path
import sys
import weakref
import duckdb
import numpy as np
import pyarrow as pa
import pyarrow.dataset as ds


def encode(value):
    if isinstance(value, (datetime.datetime, datetime.date, datetime.time)):
        return {type(value).__name__: value.isoformat()}
    if isinstance(value, decimal.Decimal):
        return {'Decimal': str(value)}
    if isinstance(value, bytes):
        return {'bytes': value.hex()}
    if isinstance(value, (list, tuple)):
        return [encode(item) for item in value]
    if isinstance(value, dict):
        return {name: encode(item) for name, item in value.items()}
    return value


outcomes = {}


def capture(name, action):
    try:
        outcomes[name] = {'value': encode(action())}
    except Exception as error:
        outcomes[name] = {'error': type(error).__name__, 'message': str(error)}


table = pa.table({'i': pa.array([1, None, 3], type=pa.int32()), 's': ['a', None, '雪']})
with duckdb.connect() as con:
    for name, obj in [('table', table), ('record_batch', table.to_batches()[0]),
                      ('reader', table.to_reader()), ('dataset', ds.dataset(table)),
                      ('scanner', ds.dataset(table).scanner())]:
        capture(name, lambda obj=obj: con.from_arrow(obj).fetchall())
    capture('metadata', lambda: [con.from_arrow(table).columns, [str(t) for t in con.from_arrow(table).types], con.from_arrow(table).type])
    capture('transform', lambda: con.from_arrow(table).filter('i > 1').project('i * 2 AS j, s').order('j').fetchall())
    relation = con.from_arrow(table)
    capture('repeat', lambda: [relation.fetchall(), relation.fetchall()])
    reader_relation = con.from_arrow(table.to_reader())
    capture('reader_repeat', lambda: [reader_relation.fetchall(), reader_relation.fetchall()])
    for name, array in {
        'nested': pa.array([[1, None], None, []], type=pa.list_(pa.int32())),
        'dictionary': pa.array(['b', 'a', None]).dictionary_encode(),
        'decimal': pa.array([decimal.Decimal('12.30'), None], type=pa.decimal128(9, 2)),
        'timestamp_ns': pa.array([-1, 0, None], type=pa.timestamp('ns')),
        'large_string': pa.array(['a', None, '雪'], type=pa.large_string()),
        'binary': pa.array([b'a\0b\xff', None], type=pa.binary()),
    }.items():
        capture(name, lambda array=array: con.from_arrow(pa.table({'v': array})).fetchall())
    capture('empty', lambda: con.from_arrow(table.slice(0, 0)).fetchall())
    capture('invalid', lambda: con.from_arrow(42))


def live_buffer():
    with duckdb.connect() as con:
        array = np.arange(4, dtype='int64')
        relation = con.from_arrow(pa.table({'i': array}))
        array[0] = 99
        first = relation.fetchall()
        array[1] = 88
        return [first, relation.fetchall()]


def retained_owner():
    with duckdb.connect() as con:
        table = pa.table({'i': [1, 2, 3]})
        reference = weakref.ref(table)
        relation = con.from_arrow(table)
        del table
        gc.collect()
        first = reference() is not None
        derived = relation.project('i + 1 AS j')
        del relation
        gc.collect()
        second = reference() is not None
        rows = derived.fetchall()
        del derived
        con.execute('SELECT 1').fetchall()
        gc.collect()
        return [first, second, rows, reference() is None]


capture('live_buffer', live_buffer)
capture('retained_owner', retained_owner)


class ProtocolInput:
    def __init__(self, expose_schema=False, single_use=False):
        self.table = pa.table({'i': [1, 2, 3]})
        if expose_schema:
            self.schema = self.table.schema
        self.calls = 0
        self.single_use = single_use

    def __arrow_c_stream__(self, requested_schema=None):
        self.calls += 1
        if self.single_use and self.calls > 1:
            raise RuntimeError('Stream already consumed')
        return self.table.__arrow_c_stream__(requested_schema=requested_schema)


def protocol(expose_schema, single_use):
    with duckdb.connect() as con:
        obj = ProtocolInput(expose_schema, single_use)
        relation = con.from_arrow(obj)
        initial = obj.calls
        rows = relation.fetchall()
        return [initial, rows, obj.calls]


for exposed in [False, True]:
    for single in [False, True]:
        capture(f'protocol_schema_{exposed}_single_{single}', lambda exposed=exposed, single=single: protocol(exposed, single))


def registration():
    with duckdb.connect() as con:
        table = pa.table({'i': [1, 2, 3]})
        reference = weakref.ref(table)
        same = con.register('registered;table', table) is con
        del table
        gc.collect()
        alive = reference() is not None
        rows = con.sql('SELECT * FROM "registered;table"').fetchall()
        same_unregister = con.unregister('registered;table') is con
        con.execute('SELECT 1').fetchall()
        gc.collect()
        released = reference() is None
        return [same, alive, rows, same_unregister, released, con.unregister('missing') is con]


def view_dependency():
    with duckdb.connect() as con:
        table = pa.table({'i': [1, 2, 3]})
        reference = weakref.ref(table)
        con.from_arrow(table).create_view('owned_view')
        del table
        gc.collect()
        alive = reference() is not None
        rows = con.sql('SELECT * FROM owned_view').fetchall()
        con.execute('DROP VIEW owned_view; SELECT 1').fetchall()
        gc.collect()
        return [alive, rows, reference() is None]


capture('registration', registration)
capture('view_dependency', view_dependency)


def captured_frame_relation():
    with duckdb.connect() as con:
        def make():
            obj = pa.table({'i': [1, 2, 3]})
            return con.sql('SELECT obj.i FROM obj WHERE i > 1')
        relation = make()
        gc.collect()
        return [relation.fetchall(), relation.project('i + 1 AS j').fetchall()]


def captured_sql_view():
    with duckdb.connect() as con:
        obj = pa.table({'i': [1, 2, 3]})
        reference = weakref.ref(obj)
        con.execute('CREATE VIEW frame_view AS SELECT * FROM obj')
        del obj
        gc.collect()
        alive = reference() is not None
        rows = con.sql('SELECT * FROM frame_view').fetchall()
        con.execute('DROP VIEW frame_view; SELECT 1').fetchall()
        gc.collect()
        return [alive, rows, reference() is None]


def module_frame():
    obj = pa.table({'i': [1, 2, 3]})
    return duckdb.sql('SELECT * FROM obj').fetchall()


def caller_frame(scan_all):
    with duckdb.connect() as con:
        outer = pa.table({'i': [1, 2, 3]})
        con.execute(f'SET python_scan_all_frames={str(scan_all).lower()}')
        def inner():
            return con.sql('SELECT * FROM outer').fetchall()
        return inner()


def disabled_replacement():
    with duckdb.connect() as con:
        obj = pa.table({'i': [1, 2, 3]})
        con.execute('SET python_enable_replacements=false')
        return con.sql('SELECT * FROM obj').fetchall()


def catalog_precedence():
    with duckdb.connect() as con:
        con.execute('CREATE TABLE obj AS SELECT 42 AS i')
        obj = pa.table({'i': [1, 2, 3]})
        return con.sql('SELECT * FROM obj').fetchall()


capture('captured_frame_relation', captured_frame_relation)
capture('captured_sql_view', captured_sql_view)
capture('module_frame', module_frame)
capture('caller_frame_default', lambda: caller_frame(False))
capture('caller_frame_enabled', lambda: caller_frame(True))
capture('disabled_replacement', disabled_replacement)
capture('catalog_precedence', catalog_precedence)
Path(sys.argv[1]).write_text(json.dumps({'version': duckdb.__version__, 'outcomes': outcomes}, indent=2) + '\n')
print(json.dumps({'outcomes': len(outcomes)}))
