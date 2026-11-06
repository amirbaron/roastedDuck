"""Relational catalog metadata preserves streams, transactions and decoder errors."""
import json
from pathlib import Path
import re
import sys

import duckdb
import pandas as pd


def failure(error):
    result = {'error': type(error).__name__,
              'message': re.sub(r'(?<= at )0x[0-9a-fA-F]+', '0xADDR', str(error))}
    if isinstance(error, UnicodeDecodeError):
        result.update(encoding=error.encoding, object_hex=error.object.hex(),
                      start=error.start, end=error.end, reason=error.reason)
    return result


results = {}
con = duckdb.connect(config={'threads': 1})
con.execute('CREATE TABLE physical AS SELECT 42 i')
con.execute('CREATE VIEW virtual AS SELECT * FROM physical')
for method in ['table', 'view']:
    for name in ['physical', b'physical', 'virtual', b'virtual']:
        relation = getattr(con, method)(name)
        results[f'{method}/{name!r}'] = {'rows': relation.fetchall(), 'type': relation.type,
                                       'columns': relation.columns}
con.execute('SELECT i FROM range(10001) t(i)')
assert con.fetchone() == (0,)
for expected, method, name in [(1, 'table', b'physical'), (2, 'table', b'virtual'), (3, 'view', b'virtual')]:
    relation = getattr(con, method)(name)
    assert relation.columns == ['i']
    row = con.fetchone()
    assert row == (expected,)
    results[f'stream/{method}/{expected}'] = row
remaining = con.fetchall()
assert len(remaining) == 9997 and sum(row[0] for row in remaining) == sum(range(4, 10001))
results['stream/remainder'] = [len(remaining), sum(row[0] for row in remaining)]
con.begin()
con.execute('INSERT INTO physical VALUES (99)')
assert con.table(b'physical').columns == ['i']
assert con.table(b'virtual').columns == ['i']
con.rollback()
results['transaction/rollback'] = con.execute('SELECT * FROM physical').fetchall()
assert results['transaction/rollback'] == [(42,)]
name = bytearray(b'before')
con.register(name, pd.DataFrame({'i': [7]}))
name[:] = b'after'
results['registration/name_copy'] = con.view(b'before').fetchall()
assert results['registration/name_copy'] == [(7,)]
con.unregister(b'before')
for json_errors in [False, True]:
    con.execute(f'SET errors_as_json={str(json_errors).lower()}')
    for replacements in [False, True]:
        con.execute(f'SET python_enable_replacements={str(replacements).lower()}')
        try:
            con.view(b'missing\xff').fetchall()
            raise AssertionError('missing view succeeded')
        except Exception as error:
            results[f'decoder/json={json_errors}/replacements={replacements}'] = failure(error)
con.close()
Path(sys.argv[1]).write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({'cases': len(results)}))
