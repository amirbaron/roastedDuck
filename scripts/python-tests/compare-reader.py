"""Arrow reader/capsule lifetime and streaming outcomes against the frozen C++ client."""
import gc
import hashlib
import json
from pathlib import Path
import sys
import warnings
import weakref
from concurrent.futures import ThreadPoolExecutor
import duckdb
import pyarrow as pa


def summarize(reader):
    batches = list(reader)
    rows = [row for batch in batches for row in batch.to_pylist()]
    return {'schema': str(reader.schema), 'batches': [len(batch) for batch in batches],
            'rows': len(rows), 'hash': hashlib.sha256(json.dumps(rows, sort_keys=True).encode()).hexdigest(),
            'sample': rows[:1] + rows[-1:]}


outcomes = {}


def encode(value):
    if isinstance(value, duckdb.sqltypes.DuckDBPyType):
        return {'sqltype': str(value)}
    if isinstance(value, (list, tuple)):
        return [encode(item) for item in value]
    if isinstance(value, dict):
        return {key: encode(item) for key, item in value.items()}
    return value


def capture(name, action):
    try:
        outcomes[name] = {'value': encode(action())}
    except Exception as error:
        outcomes[name] = {'error': type(error).__name__, 'message': str(error)}


def basic(kind, size, materialized=False):
    with duckdb.connect() as owner:
        query = 'SELECT * FROM range(5000) t(i)'
        result = owner.execute(query) if kind == 'connection' else owner.sql(query)
        if materialized:
            result.execute()
        return summarize(result.to_arrow_reader(size))


for kind in ['connection', 'relation']:
    for size in [1, 2048, 2049, 10000]:
        capture(f'{kind}_{size}', lambda kind=kind, size=size: basic(kind, size))
capture('materialized', lambda: basic('relation', 2049, True))


def owner_lifetime(close=False, capsule=False):
    owner = duckdb.connect()
    relation = owner.sql('SELECT * FROM range(5000) t(i)')
    result = relation.__arrow_c_stream__() if capsule else relation.to_arrow_reader(2049)
    del relation
    if close:
        owner.close()
    del owner
    gc.collect()
    if capsule:
        result = pa.RecordBatchReader._import_from_c_capsule(result)
    return summarize(result)


capture('owner_deleted', owner_lifetime)
capture('owner_closed', lambda: owner_lifetime(True))
capture('capsule_owner_deleted', lambda: owner_lifetime(capsule=True))
capture('capsule_owner_closed', lambda: owner_lifetime(True, True))


def interleave():
    with duckdb.connect() as owner:
        reader = owner.sql('SELECT * FROM range(5000) t(i)').to_arrow_reader(1)
        first = reader.read_next_batch().to_pylist()
        other = owner.execute('SELECT 99').fetchall()
        return [first, other, summarize(reader)]


capture('interleave', interleave)
with duckdb.connect() as owner:
    for method in ['to_arrow_reader', 'arrow', 'fetch_record_batch']:
        with warnings.catch_warnings(record=True) as seen:
            warnings.simplefilter('always')
            capture('alias_' + method, lambda method=method: summarize(getattr(owner.sql('SELECT 42 AS i'), method)(10)))
            outcomes['warnings_' + method] = {'value': [[type(w.message).__name__, str(w.message)] for w in seen]}
    reader = owner.execute('SELECT 42 AS i').to_arrow_reader()
    capture('connection_fetch_after_transfer', owner.fetchall)
    capture('connection_reader_after_transfer', owner.to_arrow_reader)
    capture('connection_description_after_transfer', lambda: owner.description)
    capture('connection_reader_value', lambda: summarize(reader))


def cross_connection():
    with duckdb.connect() as first, duckdb.connect() as second:
        reader = first.sql('SELECT * FROM range(5000) t(i)').to_arrow_reader(2049)
        return second.sql('SELECT count(*), sum(i) FROM reader').fetchall()


capture('cross_connection', cross_connection)


def deferred_error():
    with duckdb.connect() as owner:
        reader = owner.sql("SELECT CASE WHEN i >= 2048 THEN error('later') ELSE i END AS i FROM range(5000) t(i)").to_arrow_reader(1)
        first = reader.read_next_batch().to_pylist()
        try:
            reader.read_all()
        except Exception as error:
            return [first, type(error).__name__, str(error)]
        raise AssertionError('deferred error disappeared')


capture('deferred_error', deferred_error)


def interrupted():
    with duckdb.connect() as owner:
        reader = owner.sql('SELECT * FROM range(5000) t(i)').to_arrow_reader(1)
        owner.interrupt()
        try:
            first = reader.read_next_batch().to_pylist()
            result = ['rows', first]
        except Exception as error:
            result = [type(error).__name__, str(error)]
        recovered = owner.execute('SELECT 42').fetchall()
        return [result, recovered]


def threaded():
    owner = duckdb.connect()
    reader = owner.sql('SELECT * FROM range(5000) t(i)').to_arrow_reader(2049)
    del owner
    gc.collect()
    with ThreadPoolExecutor(max_workers=1) as executor:
        return executor.submit(summarize, reader).result(timeout=10)


class Input:
    def __init__(self):
        self.table = pa.table({'i': list(range(5000))})
        self.schema = self.table.schema

    def __arrow_c_stream__(self, requested_schema=None):
        return self.table.__arrow_c_stream__(requested_schema=requested_schema)


def early_release():
    with duckdb.connect() as owner:
        source = Input()
        reference = weakref.ref(source)
        relation = owner.from_arrow(source)
        reader = relation.to_arrow_reader(1)
        del relation, source
        gc.collect()
        retained = reference() is not None
        first = reader.read_next_batch().to_pylist()
        reader.close()
        del reader
        owner.execute('SELECT 1').fetchall()
        gc.collect()
        return [retained, first, reference() is None, owner.execute('SELECT 42').fetchall()]


def repeated_relation():
    with duckdb.connect() as owner:
        relation = owner.sql('SELECT * FROM range(3) t(i)')
        first = relation.to_arrow_reader(1)
        second = relation.to_arrow_reader(1)
        return [summarize(first), summarize(second)]


capture('interrupted', interrupted)
capture('threaded', threaded)
capture('early_release', early_release)
capture('repeated_relation', repeated_relation)
Path(sys.argv[1]).write_text(json.dumps({'version': duckdb.__version__, 'outcomes': outcomes}, indent=2) + '\n')
print(json.dumps({'outcomes': len(outcomes)}))
