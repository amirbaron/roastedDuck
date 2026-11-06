"""Compare typed fetch/cursor boundaries using a hash-pinned installed engine.

Usage: python fetch-conversion-boundaries.py CONFIG.json
Config keys: engine (cpp/rust), native_path, native_sha256, report.
The caller authenticates the selected native artifact before invoking this worker.
All420 cases run; output retains complete typed values and errors.
"""
from pathlib import Path
import datetime, decimal, importlib, json, math, sys
from native_hashing import sha256_stream
from native_identity import loaded_engine_images

config = json.loads(Path(sys.argv[1]).read_text())
path = Path(config['native_path']).resolve()
with path.open('rb') as stream:
    assert sha256_stream(stream) == config['native_sha256']
import duckdb
import numpy as np
import pandas as pd
native = importlib.import_module('duckdb._duckdb' if config['engine'] == 'rust' else '_duckdb')
assert Path(native.__file__).resolve() == path
assert bool(getattr(native, '__rust_engine__', False)) == (config['engine'] == 'rust')
assert duckdb.__version__ == '1.5.6'
before = loaded_engine_images(path)

def encode(value):
    if value is None:
        return None
    if value is pd.NA or value is pd.NaT:
        return {'missing': type(value).__name__}
    if isinstance(value, np.ma.MaskedArray):
        return {'masked_array': True, 'dtype': str(value.dtype), 'shape': list(value.shape),
                'mask': np.ma.getmaskarray(value).tolist(), 'values': encode(value.tolist())}
    if isinstance(value, np.ndarray):
        return {'array': True, 'dtype': str(value.dtype), 'shape': list(value.shape), 'values': encode(value.tolist())}
    if isinstance(value, np.generic):
        return encode(value.item())
    if isinstance(value, pd.DataFrame):
        return {'dataframe': True, 'columns': list(value.columns), 'dtypes': [str(dtype) for dtype in value.dtypes],
                'shape': list(value.shape), 'rows': encode(list(value.itertuples(index=False, name=None)))}
    if isinstance(value, (datetime.datetime, datetime.date, datetime.time)):
        return {'type': type(value).__name__, 'isoformat': value.isoformat()}
    if isinstance(value, datetime.timedelta):
        return {'timedelta': [value.days, value.seconds, value.microseconds]}
    if isinstance(value, decimal.Decimal):
        return {'decimal': str(value)}
    if isinstance(value, bytes):
        return {'bytes_hex': value.hex()}
    if isinstance(value, float) and not math.isfinite(value):
        return {'float': repr(value)}
    if isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, dict):
        return {str(key): encode(item) for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [encode(item) for item in value]
    raise AssertionError(('Unhandled observation type', type(value).__module__, type(value).__qualname__, repr(value)))

def observe(fn):
    try:
        value = fn()
    except Exception as error:
        return {'status': 'error', 'class': type(error).__name__, 'message': str(error)}
    return {'status': 'value', 'value': encode(value)}

queries = [
    ('timetz-value', "SELECT '12:34:56.123456+02:30'::TIMETZ AS x"),
    ('timetz-empty', "SELECT '12:34:56.123456+02:30'::TIMETZ AS x WHERE false"),
    ('timetz-null', 'SELECT NULL::TIMETZ AS x'),
    ('timetz-mixed', "SELECT x FROM (VALUES (1,'12:34:56.123456+02:30'::TIMETZ),(2,NULL::TIMETZ),(3,'01:02:03-05:00'::TIMETZ))t(i,x) ORDER BY i"),
    ('time-control', "SELECT '12:34:56.123456'::TIME AS x"),
    ('time-ns-value', "SELECT '12:34:56.123456789'::TIME_NS AS x"),
    ('time-ns-empty', "SELECT '12:34:56.123456789'::TIME_NS AS x WHERE false"),
    ('uhugeint-value', 'SELECT 340282366920938463463374607431768211455::UHUGEINT AS x'),
    ('uhugeint-empty', 'SELECT NULL::UHUGEINT AS x WHERE false'),
    ('bignum-value', "SELECT '12345678901234567890123456789012345678901234567890'::BIGNUM AS x"),
    ('bignum-empty', 'SELECT NULL::BIGNUM AS x WHERE false'),
    ('sqlnull-control', 'SELECT NULL AS x'),
    ('aggregate-state-value', 'SELECT sum(i) EXPORT_STATE AS x FROM range(3)t(i)'),
    ('aggregate-state-empty', 'SELECT x FROM (SELECT sum(i) EXPORT_STATE AS x FROM range(3)t(i))t WHERE false'),
]
records = []
for name, sql in queries:
    for surface in ['connection', 'relation', 'relation-materialized']:
        for method in ['fetchone', 'fetchmany', 'fetchall', 'fetchnumpy', 'df']:
            con = duckdb.connect(config={'autoinstall_known_extensions': 'false', 'autoload_known_extensions': 'false'})
            target = con
            try:
                def execute():
                    global target
                    if surface == 'connection':
                        con.execute(sql)
                    else:
                        target = con.sql(sql)
                        if surface == 'relation-materialized':
                            target.execute()
                    return None
                executed = observe(execute)
                action = (lambda: target.fetchmany(1)) if method == 'fetchmany' else getattr(target, method)
                result = observe(action) if executed['status'] == 'value' else None
                following = observe(target.fetchall)
                records.append({'id': surface + '-' + name + '-' + method, 'surface': surface,
                                'sql': sql, 'method': method, 'execute': executed,
                                'result': result, 'following_fetchall': following})
            finally:
                con.close()
for name, sql in queries:
    for surface in ['connection', 'relation', 'relation-materialized']:
        for sequence in [('fetchone', 'fetchnumpy', 'fetchall'), ('fetchmany', 'df', 'fetchall'), ('fetchone', 'fetchall', 'fetchone'), ('fetchnumpy', 'fetchnumpy', 'fetchall'), ('df', 'df', 'fetchall')]:
            con = duckdb.connect(config={'autoinstall_known_extensions': 'false', 'autoload_known_extensions': 'false'})
            target = con
            try:
                def execute():
                    global target
                    if surface == 'connection':
                        con.execute(sql)
                    else:
                        target = con.sql(sql)
                        if surface == 'relation-materialized':
                            target.execute()
                    return None
                executed = observe(execute)
                outcomes = []
                if executed['status'] == 'value':
                    for operation in sequence:
                        action = (lambda: target.fetchmany(1)) if operation == 'fetchmany' else getattr(target, operation)
                        outcomes.append({'operation': operation, 'outcome': observe(action)})
                records.append({'id': surface + '-' + name + '-sequence-' + '-'.join(sequence), 'surface': surface,
                                'sql': sql, 'sequence': list(sequence), 'execute': executed, 'outcomes': outcomes})
            finally:
                con.close()
assert len(records) == 420
after = loaded_engine_images(path)
assert before == after == [str(path)]
with path.open('rb') as stream:
    assert sha256_stream(stream) == config['native_sha256']
Path(config['report']).write_text(json.dumps({'engine': config['engine'], 'native_sha256': config['native_sha256'],
    'engine_images_before': before, 'engine_images_after': after, 'records': records}, indent=2) + '\n')
