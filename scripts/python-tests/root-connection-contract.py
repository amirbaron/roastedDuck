"""Strict root/native helper argument boundaries, including false-valued connections."""
import json
from pathlib import Path
import re
import sys

import duckdb
import _duckdb

output = Path(sys.argv[1])
class SpoofConnection:
    @property
    def __class__(self):
        return duckdb.DuckDBPyConnection

calls = {
    'sql': ('SELECT 42',), 'query': ('SELECT 42',), 'from_query': ('SELECT 42',),
    'execute': ('SELECT 42',), 'fetchone': (), 'fetchall': (), 'fetchmany': (1,),
    'description': (), 'rowcount': (), 'cursor': (), 'duplicate': (),
    'query_progress': (), 'values': (42,), 'table_function': ('range', [1]),
    'extract_statements': ('SELECT 42',), 'get_table_names': ('SELECT 42',),
    'list_filesystems': (), 'filesystem_is_registered': ('_missing',),
}
wrong = {'false': False, 'zero': 0, 'empty_list': [], 'empty_dict': {},
         'empty_string': '', 'true': True, 'one': 1, 'list': [1],
         'dict': {'x': 1}, 'string': 'bad', 'object': object(),
         'spoof': SpoofConnection()}
results = {}
for surface, module in [('root', duckdb), ('native', _duckdb)]:
    for name, args in calls.items():
        for label, connection in wrong.items():
            key = f'{surface}/{name}/{label}'
            duckdb.default_connection().execute('SELECT 42')
            try:
                value = getattr(module, name)(*args, connection=connection)
                results[key] = {'returned': type(value).__name__}
                if isinstance(value, duckdb.DuckDBPyConnection):
                    value.close()
            except Exception as error:
                results[key] = {'error': type(error).__name__,
                                'message': re.sub(r'0x[0-9a-fA-F]+', '0xADDR', str(error))}
output.write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({'cases': len(results), 'type_errors': sum(x.get('error') == 'TypeError' for x in results.values())}))
