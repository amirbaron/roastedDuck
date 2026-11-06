"""Raw-byte and typed-name journeys for frozen connection/relation APIs."""
import json
from pathlib import Path
import re
import sys

import duckdb
import _duckdb


def failure(error):
    return {'error': type(error).__name__,
            'message': re.sub(r'(?<= at )0x[0-9a-fA-F]+', '0xADDR', str(error))}


names = {f'byte_{value:02x}': b'n' + bytes([value]) for value in range(256)}
names.update({'text': 'alias', 'unicode': 'café', 'utf8': b'caf\xc3\xa9',
              'bytearray': bytearray(b'alias'), 'invalid_bytearray': bytearray(b'\xff'),
              'surrogate': 'x\ud800', 'none': None, 'int': 1, 'bool': False,
              'list': [], 'object': object()})
con = duckdb.connect(config={'threads': 1})
results = {}
for label, name in names.items():
    for scope, owner in [('connection', con), ('root', duckdb), ('native', _duckdb)]:
        for method in ['sql', 'query', 'from_query']:
            key = f'{scope}/{method}/{label}'
            try:
                kwargs = {'alias': name}
                if scope != 'connection':
                    kwargs['connection'] = con
                relation = getattr(owner, method)('SELECT 42', **kwargs)
                outcome = {'rows': relation.fetchall()}
                try:
                    outcome['alias'] = relation.alias
                except Exception as error:
                    outcome['alias_error'] = failure(error)
                results[key] = outcome
            except Exception as error:
                results[key] = failure(error)
    base = con.sql('SELECT 42')
    try:
        relation = base.set_alias(name)
        outcome = {'rows': relation.fetchall()}
        try:
            outcome['alias'] = relation.alias
        except Exception as error:
            outcome['alias_error'] = failure(error)
        results[f'relation/set_alias/{label}'] = outcome
    except Exception as error:
        results[f'relation/set_alias/{label}'] = failure(error)
con.close()
Path(sys.argv[1]).write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({'cases': len(results), 'rows_returned': sum('rows' in item for item in results.values())}))
