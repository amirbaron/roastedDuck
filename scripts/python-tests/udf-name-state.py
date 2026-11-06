"""Scalar UDF name snapshots, case folding, owners and closed-connection errors."""
import gc
import json
from pathlib import Path
import re
import sys
import weakref
import duckdb


class Callback:
    def __call__(self, value):
        return value + 1


def failure(error):
    return {'error': type(error).__name__,
            'message': re.sub(r'(?<= at )0x[0-9a-fA-F]+', '0xADDR', str(error))}


results = {}
con = duckdb.connect(config={'threads': 1})
callback = Callback()
ref = weakref.ref(callback)
name = bytearray(b'MiXeD')
con.create_function(name, callback, [int], int)
name[:] = b'changed'
del callback
gc.collect()
assert ref() is not None
results['copied/casefolded_call'] = con.sql('SELECT 41 i').select(
    duckdb.FunctionExpression(b'mixed', duckdb.ColumnExpression('i'))).fetchall()
assert results['copied/casefolded_call'] == [(42,)]
con.remove_function(b'MIXED')
gc.collect()
results['callback_released'] = ref() is None
for label, value in [('removed', b'mixed'), ('missing', b'absent\xff')]:
    try:
        con.remove_function(value)
        raise AssertionError('missing removal succeeded')
    except Exception as error:
        results['remove/' + label] = failure(error)
con.create_function('closed_probe', Callback(), [int], int)
con.close()
for label, method, args in [('closed/remove', 'remove_function', (b'closed_probe',)),
                           ('closed/remove_missing', 'remove_function', ('absent',)),
                           ('closed/create', 'create_function', (b'new', Callback(), [int], int)),
                           ('closed/create_bad', 'create_function', (None, Callback(), [int], int))]:
    try:
        getattr(con, method)(*args)
        raise AssertionError('closed operation succeeded')
    except Exception as error:
        results[label] = failure(error)
Path(sys.argv[1]).write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({'cases': len(results), 'callback_released': results['callback_released']}))
