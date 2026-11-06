"""Frozen C++ vs native Rust dataframe/NumPy scans, including retained/live arrays."""
import datetime
import decimal
import gc
import json
from pathlib import Path
import sys
import weakref
import duckdb
import numpy as np
import pandas as pd


def encode(value):
    if value is None or value is pd.NA or value is pd.NaT:
        return None
    if isinstance(value, (datetime.datetime, datetime.date, datetime.time)):
        return {type(value).__name__: value.isoformat()}
    if isinstance(value, datetime.timedelta):
        return {'timedelta': [value.days, value.seconds, value.microseconds]}
    if isinstance(value, decimal.Decimal):
        return {'Decimal': str(value)}
    if isinstance(value, float) and not np.isfinite(value):
        return {'float': str(value)}
    if isinstance(value, bytes):
        return {'bytes': value.hex()}
    if isinstance(value, (list, tuple)):
        return [encode(item) for item in value]
    if isinstance(value, dict):
        return {key: encode(item) for key, item in value.items()}
    return value


def result(relation):
    return [relation.columns, [str(t) for t in relation.types], relation.fetchall()]


outcomes = {}


def capture(name, action):
    try:
        outcomes[name] = {'value': encode(action())}
    except Exception as error:
        outcomes[name] = {'error': type(error).__name__, 'message': str(error)}


with duckdb.connect() as con:
    for dtype, values in {
        'bool': [True, False], 'int8': [-8, 7], 'int16': [-32768, 32767],
        'int32': [-2147483648, 2147483647], 'int64': [-9223372036854775808, 9223372036854775807],
        'uint8': [0, 255], 'uint16': [0, 65535], 'uint32': [0, 4294967295],
        'uint64': [0, 18446744073709551615], 'float16': [1.5, np.nan],
        'float32': [1.5, np.nan], 'float64': [1.5, np.nan],
        'Int8': [-8, None], 'Int16': [-32768, None], 'Int32': [-2147483648, None],
        'Int64': [-9223372036854775808, None], 'UInt64': [18446744073709551615, None],
        'boolean': [True, None], 'Float64': [1.5, None], 'string': ['a', None],
    }.items():
        frame = pd.DataFrame({'v': pd.Series(values, dtype=dtype)})
        capture('dtype_' + dtype, lambda frame=frame: result(con.from_df(frame)))
    for name, series in {
        'object_int': pd.Series([1, None, 3], dtype=object),
        'object_mixed': pd.Series([1, 'x', True], dtype=object),
        'object_lists': pd.Series([[1, None], None, []], dtype=object),
        'object_structs': pd.Series([{'a': 1}, {'a': None}], dtype=object),
        'object_varying_keys': pd.Series([{'a': 1}, {'b': 2}], dtype=object),
        'object_nested_maps': pd.Series([[{'a': 1}], [{'b': 2}]], dtype=object),
        'object_decimal': pd.Series([decimal.Decimal('12.30'), None], dtype=object),
        'object_decimal_exponent': pd.Series([decimal.Decimal('1E5')], dtype=object),
        'object_decimal_special': pd.Series([decimal.Decimal('-Infinity')], dtype=object),
        'object_date': pd.Series([datetime.date(2024, 1, 2), None], dtype=object),
        'object_nulls': pd.Series([None, pd.NA, pd.NaT, np.nan], dtype=object),
        'category_string': pd.Series(pd.Categorical(['b', 'a', None], categories=['b', 'a'])),
        'category_number': pd.Series(pd.Categorical([3, 1, None])),
        'timestamp_ns': pd.Series(np.array([-1, 0, np.iinfo('int64').min], dtype='datetime64[ns]')),
        'timestamp_us': pd.Series(np.array([-1, 0, np.iinfo('int64').min], dtype='datetime64[us]')),
        'timestamp_tz': pd.Series(pd.to_datetime(['2024-01-02', None], utc=True)),
        'timedelta': pd.Series(pd.to_timedelta([1, -1, None], unit='ns')),
    }.items():
        capture(name, lambda series=series: result(con.from_df(pd.DataFrame({'v': series}))))
    duplicate = pd.DataFrame([[1, 2, 3]], columns=['A', 'a', 'a_1'])
    capture('duplicate_names', lambda: result(con.from_df(duplicate)))
    capture('empty_object', lambda: result(con.from_df(pd.DataFrame({'v': pd.Series([], dtype=object)}))))
    capture('no_columns', lambda: result(con.from_df(pd.DataFrame())))
    con.execute('SET pandas_analyze_sample=0')
    capture('sampling_disabled', lambda: result(con.from_df(pd.DataFrame({'v': pd.Series([True, 42, None], dtype=object)}))))
    con.execute('SET pandas_analyze_sample=1000')
    obj = np.arange(6, dtype='int32')
    capture('numpy_1d', lambda: result(con.sql('SELECT * FROM obj')))
    obj = np.arange(6, dtype='int32').reshape(2, 3)
    capture('numpy_2d', lambda: result(con.sql('SELECT * FROM obj')))
    obj = np.array(['z', 'a', 'z'])
    capture('numpy_unicode', lambda: result(con.sql('SELECT * FROM obj')))
    obj = np.arange(6, dtype='int64')[::-2]
    capture('numpy_stride', lambda: result(con.sql('SELECT * FROM obj')))
    frame = pd.DataFrame({'v': [1, 2, 3]}, index=[7, 5, 3])
    capture('frame_lookup', lambda: result(con.sql('SELECT * FROM frame')))
    capture('query_df', lambda: result(duckdb.query_df(frame, 'x', 'SELECT v + 1 AS j FROM x', connection=con)))
    obj = np.arange(10, dtype='int32')[::3]
    capture('numpy_positive_stride', lambda: result(con.sql('SELECT * FROM obj')))
    sampled = [None] * 2100
    sampled[3] = 42
    capture('sample_first_valid', lambda: result(con.from_df(pd.DataFrame({'v': pd.Series(sampled, dtype=object)})).filter('v IS NOT NULL')))


def live_frame():
    with duckdb.connect() as con:
        frame = pd.DataFrame({'v': np.arange(4, dtype='int64')})
        relation = con.from_df(frame)
        frame.iloc[0, 0] = 99
        first = relation.fetchall()
        frame['v'] = [10, 11, 12, 13]
        frame.columns = ['new_name']
        return [first, relation.columns, relation.fetchall()]


def owner_lifetime():
    with duckdb.connect() as con:
        frame = pd.DataFrame({'v': [1, 2, 3]})
        reference = weakref.ref(frame)
        relation = con.from_df(frame)
        del frame
        gc.collect()
        alive = reference() is not None
        rows = relation.fetchall()
        del relation
        con.execute('SELECT 1').fetchall()
        gc.collect()
        return [alive, rows, reference() is None]


capture('live_frame', live_frame)
capture('owner_lifetime', owner_lifetime)
Path(sys.argv[1]).write_text(json.dumps({'version': duckdb.__version__, 'outcomes': outcomes}, indent=2) + '\n')
print(json.dumps({'outcomes': len(outcomes)}))
