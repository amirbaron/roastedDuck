"""Exact public NumPy/Pandas outcomes, run in separate C++ and Rust environments."""
import datetime
import decimal
import hashlib
import json
from pathlib import Path
import sys
import duckdb
import numpy as np
import pandas as pd
import pyarrow as pa


def encode(value):
    if isinstance(value, pa.Table):
        rows = encode(value.to_pylist())
        return {'schema': str(value.schema), 'rows': value.num_rows, 'batches': [len(batch) for batch in value.to_batches()],
                'values_sha256': hashlib.sha256(json.dumps(rows, sort_keys=True).encode()).hexdigest(),
                'sample': rows[:2] + rows[-2:]}
    if isinstance(value, decimal.Decimal):
        return {'Decimal': str(value)}
    if value is pd.NA:
        return {'NA': True}
    if value is pd.NaT:
        return {'NaT': True}
    if isinstance(value, np.ma.MaskedArray):
        return {'class': 'MaskedArray', 'dtype': str(value.dtype), 'shape': list(value.shape),
                'mask': np.ma.getmaskarray(value).tolist(),
                'values': [None if masked else encode(item) for item, masked in zip(value.data, np.ma.getmaskarray(value))],
                'writeable': bool(value.flags.writeable)}
    if isinstance(value, np.ndarray):
        return {'class': 'ndarray', 'dtype': str(value.dtype), 'shape': list(value.shape),
                'values': [encode(item) for item in value], 'writeable': bool(value.flags.writeable)}
    if isinstance(value, pd.DataFrame):
        return {'columns': list(value.columns), 'dtypes': [str(t) for t in value.dtypes],
                'values': [[encode(item) for item in row] for row in value.itertuples(index=False, name=None)],
                'categories': {name: [encode(v) for v in value[name].cat.categories] for name in value.columns if isinstance(value[name].dtype, pd.CategoricalDtype)}}
    if isinstance(value, (np.datetime64, np.timedelta64)):
        return {'dtype': str(value.dtype), 'raw': int(value.astype('int64'))}
    if isinstance(value, np.generic):
        return encode(value.item())
    if isinstance(value, (datetime.datetime, datetime.date, datetime.time)):
        return {type(value).__name__: value.isoformat()}
    if isinstance(value, datetime.timedelta):
        return {'timedelta': [value.days, value.seconds, value.microseconds]}
    if isinstance(value, bytes):
        return {'bytes': value.hex()}
    if isinstance(value, bytearray):
        return {'bytearray': value.hex()}
    if isinstance(value, float) and not np.isfinite(value):
        return {'float': str(value)}
    if isinstance(value, (list, tuple)):
        return [encode(item) for item in value]
    if isinstance(value, dict):
        return {key: encode(item) for key, item in value.items()}
    return value


outcomes = {}


def capture(name, action):
    try:
        outcomes[name] = {'value': encode(action())}
    except Exception as error:
        outcomes[name] = {'error': type(error).__name__, 'message': str(error)}


queries = {
    'integers': 'SELECT true AS b, -8::TINYINT AS i8, -16::SMALLINT AS i16, 32::INTEGER AS i32, 64::BIGINT AS i64, 8::UTINYINT AS u8, 16::USMALLINT AS u16, 32::UINTEGER AS u32, 64::UBIGINT AS u64',
    'nulls': "SELECT NULL::BOOLEAN AS b, NULL::INTEGER AS i, NULL::FLOAT AS f, NULL::VARCHAR AS s, NULL::DATE AS d, NULL::TIMESTAMP AS t, NULL::INTERVAL AS iv",
    'mixed_nulls': "SELECT * FROM (VALUES (true,1,1.25::FLOAT,'a'),(NULL,NULL,NULL,NULL),(false,3,'NaN'::FLOAT,'雪')) t(b,i,f,s)",
    'wide': 'SELECT 170141183460469231731687303715884105727::HUGEINT AS h, 340282366920938463463374607431768211455::UHUGEINT AS u, 12.30::DECIMAL(9,2) AS d',
    'temporal': "SELECT DATE '2020-01-10' AS d, TIMESTAMP '1969-12-31 23:59:59.999999' AS us, TIMESTAMP_NS '1969-12-31 23:59:59.999999999' AS ns, TIMESTAMP_MS '1969-12-31 23:59:59.999' AS ms, TIMESTAMP_S '1969-12-31 23:59:59' AS s, TIME '13:06:40' AS tm, INTERVAL '1 month 2 days 3 microseconds' AS iv",
    'nested': "SELECT [1,NULL,3] AS l, [1,2]::INTEGER[2] AS a, {'x':42} AS s, MAP(['a'],[42]) AS m, from_hex('610062ff') AS blob",
    'enum': "SELECT * FROM (VALUES ('b'::ENUM('b','a')),('a'),(NULL)) t(e)",
    'enum_typed': "SELECT e::ENUM('b','a') AS e FROM (VALUES ('b'),('a'),(NULL)) t(e)",
    'enum_empty': "SELECT NULL::ENUM('b','a') AS e WHERE false",
    'precision': 'SELECT 170141183460469231731687303715884105727::HUGEINT AS h, 1.234567890123456789::DECIMAL(38,18) AS d',
    'date_overflow': "SELECT DATE 'infinity' AS d",
    'interval_overflow': "SELECT INTERVAL '2147483647 months' AS i",
    'bit': "SELECT '010101'::BIT AS b",
    'nested_null': 'SELECT NULL::INTEGER[] AS l, NULL::BLOB AS b, NULL::STRUCT(x INTEGER) AS s',
    'list_string_null': "SELECT ['a',NULL,'b'] AS l",
    'boundary_nulls': "SELECT CASE WHEN i=4097 THEN NULL ELSE i::INTEGER END AS i, CASE WHEN i=4097 THEN NULL ELSE 'x' END AS s FROM range(5000) t(i)",
    'duplicates': 'SELECT 1 AS A, 2 AS a, 3 AS a_1, 4 AS A',
    'empty': 'SELECT NULL::INTEGER AS i, NULL::VARCHAR AS s, NULL::DATE AS d WHERE false',
    'timezone': "SELECT TIMESTAMPTZ '2024-01-02 03:04:05+00' AS t",
}
with duckdb.connect() as con:
    con.execute("SET TimeZone='Asia/Jerusalem'")
    for name, sql in queries.items():
        capture('numpy_' + name, lambda sql=sql: con.execute(sql).fetchnumpy())
        capture('pandas_' + name, lambda sql=sql: con.execute(sql).fetchdf())
        capture('arrow_' + name, lambda sql=sql: con.execute(sql).to_arrow_table())
    capture('date_as_object', lambda: con.execute("SELECT DATE '2020-01-10' AS d").fetchdf(date_as_object=True))
    capture('repeat_numpy', lambda: [con.execute('SELECT 42 AS i').fetchnumpy(), con.fetchnumpy()])
    capture('repeat_df', lambda: [con.execute('SELECT 42 AS i').df(), con.df(), con.df()])
    capture('relation_numpy_repeat', lambda: [con.sql('SELECT 42 AS i').fetchnumpy() for _ in range(2)])
    relation = con.sql('SELECT 42 AS i')
    capture('same_relation_numpy_repeat', lambda: [relation.fetchnumpy(), relation.fetchnumpy()])
    for size in [3, 5000]:
        def mixed_fetch(size=size):
            con.execute(f'SELECT * FROM range({size}) t(i)')
            first = con.fetchone()
            data = con.fetchnumpy()['i']
            return [first, len(data), data[:2].tolist(), data[-2:].tolist()]
        capture('fetchone_numpy_' + str(size), mixed_fetch)
    capture('repeat_arrow', lambda: [con.execute('SELECT 42 AS i').to_arrow_table(), con.to_arrow_table()])
    for batch_size in [1, 2048, 2049]:
        capture('arrow_batches_' + str(batch_size), lambda batch_size=batch_size: con.execute('SELECT * FROM range(5000) t(i)').to_arrow_table(batch_size))


def retained_buffers(method):
    connection = duckdb.connect()
    result = getattr(connection.execute('SELECT i, i % 3 == 0 AS b FROM range(5000) t(i)'), method)()
    connection.close()
    return result


capture('arrow_after_close', lambda: retained_buffers('to_arrow_table'))
capture('numpy_after_close', lambda: {name: [len(data), data[0].item(), data[-1].item()] for name, data in retained_buffers('fetchnumpy').items()})


def result_settings(method):
    with duckdb.connect() as owner:
        owner.execute("SET TimeZone='UTC'; SET arrow_large_buffer_size=false")
        relation = owner.sql("SELECT TIMESTAMPTZ '2024-01-02 03:04:05+00' AS t, '雪' AS s").execute()
        owner.execute("SET TimeZone='Asia/Jerusalem'; SET arrow_large_buffer_size=true")
        return getattr(relation, method)()


for method in ['fetchone', 'fetchdf', 'to_arrow_table']:
    capture('snapshot_' + method, lambda method=method: result_settings(method))


def dataframe_chunks(relation=False, materialized=False):
    with duckdb.connect() as owner:
        query = 'SELECT * FROM range(5000) t(i)'
        result = owner.sql(query) if relation else owner.execute(query)
        if materialized:
            result.execute()
        output = []
        for count in [2, 0, 1, 3, 1]:
            frame = result.fetch_df_chunk(count)
            output.append([len(frame), str(frame.dtypes.iloc[0]), frame['i'].iloc[:1].tolist(), frame['i'].iloc[-1:].tolist()])
        return output


capture('dataframe_chunks_connection', dataframe_chunks)
capture('dataframe_chunks_relation', lambda: dataframe_chunks(True))
capture('dataframe_chunks_materialized', lambda: dataframe_chunks(True, True))
Path(sys.argv[1]).write_text(json.dumps({'version': duckdb.__version__, 'outcomes': outcomes}, indent=2) + '\n')
print(json.dumps({'outcomes': len(outcomes)}))
