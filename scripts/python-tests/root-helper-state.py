"""Real helper selection, frame lookup and public exception-state journeys."""
import json
from pathlib import Path
import sys

import duckdb
import _duckdb
import pandas as pd

results = {}
native_lazy = bool(getattr(_duckdb, '__rust_engine__', False))
if native_lazy:
    assert duckdb._default is None
for label, fn in [
    ('missing_query', lambda: duckdb.sql()),
    ('missing_execute', lambda: duckdb.execute()),
    ('extra_argument', lambda: duckdb.sql('SELECT 42', 'extra')),
    ('unexpected_keyword', lambda: duckdb.sql('SELECT 42', bad_keyword=True)),
    ('bad_connection', lambda: duckdb.sql('SELECT 42', connection=False)),
    ('none_default_setter', lambda: duckdb.set_default_connection(connection=None)),
]:
    try:
        fn()
        raise AssertionError(label)
    except TypeError as error:
        results[label] = {'error': type(error).__name__,
                          'context': type(error.__context__).__name__ if error.__context__ else None,
                          'cause': type(error.__cause__).__name__ if error.__cause__ else None,
                          'suppress_context': error.__suppress_context__}
        assert error.__context__ is None and error.__cause__ is None
if native_lazy:
    assert duckdb._default is None

default = duckdb.connect(config={'threads': 1})
explicit = duckdb.connect(config={'threads': 1})
for con, value in [(default, 1), (explicit, 2)]:
    con.execute(f'CREATE TABLE _scope AS SELECT {value} i')
duckdb.set_default_connection(default)
duckdb.DuckDBPyConnection.__bool__ = lambda self: False
try:
    assert not default and not explicit
    for surface, module in [('root', duckdb), ('native', _duckdb)]:
        for name in ['sql', 'query', 'from_query', 'execute']:
            rows = getattr(module, name)('SELECT * FROM _scope', connection=explicit).fetchall()
            assert rows == [(2,)]
            results[f'{surface}/{name}/false_valid'] = rows
        rows = module.table('_scope', connection=explicit).fetchall()
        assert rows == [(2,)]
        results[f'{surface}/table/false_valid'] = rows
        default.execute('SELECT 1')
        explicit.execute('SELECT 2')
        row = module.fetchone(connection=explicit)
        assert row == (2,)
        results[f'{surface}/fetchone/false_valid'] = row
        rows = module.sql('SELECT * FROM _scope').fetchall()
        assert rows == [(1,)]
        results[f'{surface}/default_still_selected'] = rows
        frame = pd.DataFrame({'i': [10]})
        rows = module.query_df(frame, '_input', 'SELECT * FROM _scope', connection=explicit).fetchall()
        assert rows == [(2,)]
        results[f'{surface}/query_df/false_valid'] = rows
        rows = module.df(frame, connection=explicit).query('_input', 'SELECT * FROM _scope').fetchall()
        assert rows == [(2,)]
        results[f'{surface}/df/false_valid'] = rows
    explicit.close()
    try:
        duckdb.sql('SELECT * FROM _scope', connection=explicit)
        raise AssertionError('closed explicit connection was ignored')
    except duckdb.ConnectionException as error:
        results['closed_false_valid'] = {'error': type(error).__name__, 'message': str(error)}
finally:
    del duckdb.DuckDBPyConnection.__bool__
    explicit.close()
    default.close()

def replacement_frames():
    # These names also occur in the module helper implementation's local frames.
    args = pd.DataFrame({'i': [42]})
    kwargs = pd.DataFrame({'i': [43]})
    function = pd.DataFrame({'i': [44]})
    with duckdb.connect(config={'threads': 1}) as con:
        for name, expected in [('args', 42), ('kwargs', 43), ('function', 44)]:
            rows = duckdb.sql(f'SELECT * FROM {name}', connection=con).fetchall()
            assert rows == [(expected,)]
            results['replacement/' + name] = rows

replacement_frames()
Path(sys.argv[1]).write_text(json.dumps({'outcomes': results, 'native_lazy_checked': native_lazy}, indent=2) + '\n')
print(json.dumps({'cases': len(results), 'native_lazy_checked': native_lazy}))
