"""Native scalar UDF names: byte identity, expression calls, duplicate/removal rules."""
import json
from pathlib import Path
import re
import sys
import duckdb
import _duckdb


def callback(value):
    return value + 1


def failure(error):
    message = str(error)
    if type(error).__name__ == 'InternalException':
        message = message.split('\n\nStack Trace:\n', 1)[0].rstrip()
    return {'error': type(error).__name__,
            'message': re.sub(r'(?<= at )0x[0-9a-fA-F]+', '0xADDR', message)}


names = {f'byte_{i:02x}': b'n' + bytes([i]) for i in range(256)}
names.update({'text': 'probe', 'unicode': 'café', 'utf8': b'caf\xc3\xa9',
              'bytearray': bytearray(b'probe'), 'empty': '', 'dotted': 'a.b',
              'quoted': 'a"b', 'none': None, 'int': 1, 'bool': False,
              'surrogate': 'x\ud800', 'object': object()})
results = {}
for scope, module in [('connection', None), ('root', duckdb), ('native', _duckdb)]:
    for label, name in names.items():
        con = duckdb.connect(config={'threads': 1})
        owner = con if module is None else module
        extra = {} if module is None else {'connection': con}
        result = {}
        try:
            result['created'] = owner.create_function(name, callback, [int], int, **extra) is con
            try:
                expr = duckdb.FunctionExpression(name, duckdb.ColumnExpression('i'))
                result['rows'] = con.sql('SELECT 41 i').select(expr).fetchall()
            except Exception as error:
                result['call_error'] = failure(error)
            try:
                owner.create_function(name, callback, [int], int, **extra)
                result['duplicate_succeeded'] = True
            except Exception as error:
                result['duplicate_error'] = failure(error)
            result['removed'] = owner.remove_function(name, **extra) is con
            try:
                owner.remove_function(name, **extra)
                result['missing_remove_succeeded'] = True
            except Exception as error:
                result['missing_remove_error'] = failure(error)
        except Exception as error:
            result['create_error'] = failure(error)
        results[f'{scope}/{label}'] = result
        con.close()
Path(sys.argv[1]).write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({'cases': len(results)}))
