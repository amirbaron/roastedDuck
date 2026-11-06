"""Frozen module helper connection types and call binding, across all thin helpers."""
import ast
import json
from pathlib import Path
import re
import sys

import duckdb
import _duckdb

docs = json.loads(Path(sys.argv[1]).read_text())
source = ast.parse(Path(sys.argv[2]).read_text())
functions = {node.name: node for node in source.body if isinstance(node, ast.FunctionDef)}
functions['fetch_df'] = functions['fetchdf']
functions['from_parquet'] = functions['read_parquet']
wrong = {'false': False, 'zero': 0, 'empty_list': [], 'empty_dict': {},
         'empty_string': '', 'true': True, 'one': 1, 'list': [1],
         'dict': {'x': 1}, 'string': 'bad', 'object': object()}
results = {}
for surface, module in [('root', duckdb), ('native', _duckdb)]:
    for name in docs:
        cases = [(f'connection_{label}', (), {'connection': value}) for label, value in wrong.items()]
        definition = functions[name]
        positional = definition.args.posonlyargs + definition.args.args
        if len(positional) > len(definition.args.defaults):
            cases.append(('missing', (), {}))
            cases.append(('duplicate', (None,), {positional[0].arg: None}))
        if definition.args.vararg is None:
            cases.append(('extra_positional', (None,) * (len(positional) + 1), {}))
        if definition.args.kwarg is None:
            cases.append(('unexpected_keyword', (), {'_root_contract_bad': True}))
        for label, args, kwargs in cases:
            key = f'{surface}/{name}/{label}'
            try:
                value = getattr(module, name)(*args, **kwargs)
                results[key] = {'returned': type(value).__name__}
            except Exception as error:
                results[key] = {'error': type(error).__name__,
                                'message': re.sub(r'0x[0-9a-fA-F]+', '0xADDR', str(error))}
Path(sys.argv[3]).write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({'cases': len(results), 'type_errors': sum(x.get('error') == 'TypeError' for x in results.values())}))
