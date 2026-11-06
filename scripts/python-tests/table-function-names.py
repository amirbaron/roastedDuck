"""Frozen literal table-function names, byte casters and parameter/call boundaries."""
import json
from pathlib import Path
import re
import sys

import duckdb
import _duckdb

results = {}
con = duckdb.connect(config={'threads': 1})
con.execute('CREATE MACRO "main.range"(x) AS TABLE SELECT x i')
con.execute('CREATE MACRO "café"(x) AS TABLE SELECT x i')
names = {'text': 'range', 'bytes': b'range', 'bytearray': bytearray(b'range'),
         'literal_dot': 'main.range', 'literal_dot_bytes': b'main.range',
         'unicode': 'café', 'utf8': b'caf\xc3\xa9', 'empty': '', 'none': None,
         'int': 1, 'bool': False, 'surrogate': 'x\ud800', 'object': object()}
names.update({f'byte_{value:02x}': b'n' + bytes([value]) for value in range(256)})

def capture(key, action):
    try:
        relation = action()
        results[key] = {'rows': relation.fetchall(), 'columns': relation.columns, 'type': relation.type}
    except Exception as error:
        message = str(error)
        if type(error).__name__ == 'InternalException':
            message = message.split('\n\nStack Trace:\n', 1)[0].rstrip()
        results[key] = {'error': type(error).__name__,
                        'message': re.sub(r'(?<= at )0x[0-9a-fA-F]+', '0xADDR', message)}

for scope, owner in [('connection', con), ('root', duckdb), ('native', _duckdb)]:
    extra = {} if scope == 'connection' else {'connection': con}
    for label, name in names.items():
        capture(f'{scope}/name/{label}', lambda: owner.table_function(name, [2], **extra))
    for label, parameters in [('none', None), ('tuple', (2,)), ('list', [2]),
                              ('dict', {'x': 2}), ('int', 2), ('string', '2')]:
        capture(f'{scope}/parameters/{label}', lambda: owner.table_function('range', parameters, **extra))
    for label, args, kwargs in [('missing', (), {}), ('keyword', (), {'name': b'range', 'parameters': [2]}),
                              ('duplicate', ('range',), {'name': 'range'}),
                              ('extra', ('range', [2], None), {}),
                              ('unknown', ('range', [2]), {'bad': True})]:
        capture(f'{scope}/call/{label}', lambda: owner.table_function(*args, **kwargs, **extra))
con.close()
for label, name in [('valid', b'range'), ('bad', None)]:
    capture('closed/' + label, lambda: con.table_function(name, [2]))
Path(sys.argv[1]).write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({'cases': len(results)}))
