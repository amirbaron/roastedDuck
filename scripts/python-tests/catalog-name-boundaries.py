"""Frozen table/view/registration name casters and live replacement ownership."""
import json
from pathlib import Path
import re
import sys

import duckdb
import _duckdb
import pandas as pd


def failure(error):
    message = str(error)
    if type(error).__name__ == 'InternalException':
        message = message.split('\n\nStack Trace:\n', 1)[0].rstrip()
    return {'error': type(error).__name__,
            'message': re.sub(r'(?<= at )0x[0-9a-fA-F]+', '0xADDR', message)}


def relation_result(action):
    try:
        relation = action()
        result = {'rows': relation.fetchall(), 'type': relation.type}
        try:
            result['alias'] = relation.alias
        except Exception as error:
            result['alias_error'] = failure(error)
        return result
    except Exception as error:
        return failure(error)


names = {f'byte_{value:02x}': b'n' + bytes([value]) for value in range(256)}
names.update({'text': 'name', 'unicode': 'café', 'quoted': 'a"b', 'dotted': 'a.b',
              'empty_text': '', 'empty_bytes': b'', 'quoted_empty': '""',
              'utf8': b'caf\xc3\xa9', 'bytearray': bytearray(b'name'),
              'surrogate': 'x\ud800', 'none': None, 'int': 1, 'bool': False,
              'list': [], 'object': object()})
results = {}
for scope, module in [('connection', None), ('root', duckdb), ('native', _duckdb)]:
    for label, name in names.items():
        con = duckdb.connect(config={'threads': 1})
        owner = con if module is None else module
        kwargs = {} if module is None else {'connection': con}
        first = pd.DataFrame({'i': [42]})
        second = pd.DataFrame({'i': [99]})
        key = f'{scope}/{label}'
        try:
            returned = owner.register(name, first, **kwargs)
            results[key + '/register'] = {'returned_connection': returned is con}
            results[key + '/view'] = relation_result(lambda: owner.view(name, **kwargs))
            results[key + '/table'] = relation_result(lambda: owner.table(name, **kwargs))
            returned = owner.register(name, second, **kwargs)
            results[key + '/replace'] = {'returned_connection': returned is con,
                                         'view': relation_result(lambda: owner.view(name, **kwargs))}
            returned = owner.unregister(name, **kwargs)
            results[key + '/unregister'] = {'returned_connection': returned is con,
                                            'view': relation_result(lambda: owner.view(name, **kwargs))}
        except Exception as error:
            results[key + '/register_error'] = failure(error)
        con.close()
Path(sys.argv[1]).write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({'outcomes': len(results)}))
