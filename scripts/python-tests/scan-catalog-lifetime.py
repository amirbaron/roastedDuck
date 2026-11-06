"""Canonical Python scan catalog and readers surviving a second client connection."""
import gc
import json
from pathlib import Path
import sys
import weakref
import os

import duckdb
import pandas as pd
import pyarrow as pa

con = duckdb.connect(config={'threads': 1})
query = """SELECT function_name, parameter_types FROM duckdb_functions()
WHERE function_name LIKE '%scan%' AND
(function_name LIKE '%arrow%' OR function_name LIKE '%pandas%') ORDER BY 1"""
catalog = con.execute(query).fetchall()
if not os.environ.get('SCAN_CATALOG_DIAGNOSTIC'):
    assert catalog == [('arrow_scan', ['POINTER'] * 3), ('arrow_scan_dumb', ['POINTER'] * 3),
                       ('pandas_scan', ['POINTER'])], catalog
table = pa.table({'i': range(10001)})
reference = weakref.ref(table)
relation = con.from_arrow(table)
reader = relation.to_arrow_reader(1024)
first = reader.read_next_batch()
assert first.num_rows == 1024 and first.column(0)[0].as_py() == 0
other = con.cursor()
assert other.execute(query).fetchall() == catalog
frame = pd.DataFrame({'i': [10, 20]})
assert other.from_df(frame).fetchall() == [(10,), (20,)]
del table, relation
gc.collect()
assert reference() is not None
rest = reader.read_all()
values = first.column(0).to_pylist() + rest.column(0).to_pylist()
assert values == list(range(10001))
reader.close()
del reader, first, rest
gc.collect()
released_after_reader = reference() is None
if os.environ.get('SCAN_REFPOOL_DIAGNOSTIC'):
    from duckdb import _duckdb
    _duckdb._string_argument_valid('')
    gc.collect()
released_after_native_entry = reference() is None
assert other.execute('SELECT 42').fetchall() == [(42,)]
con.execute('SELECT 42').fetchall()
gc.collect()
released_after_recovery = reference() is None
other.close()
con.close()
gc.collect()
released_after_connections = reference() is None
if not os.environ.get('SCAN_CATALOG_DIAGNOSTIC') and not os.environ.get('SCAN_REFPOOL_DIAGNOSTIC'):
    assert released_after_reader and released_after_recovery and released_after_connections
Path(sys.argv[1]).write_text(json.dumps({'catalog': catalog, 'rows': len(values),
    'sum': sum(values), 'retained_until_reader_release': True,
    'released_after_reader_close': released_after_reader,
    'released_after_native_entry': released_after_native_entry,
    'released_after_recovery': released_after_recovery,
    'released_after_connections': released_after_connections,
    'new_connection_while_streaming': True}, indent=2) + '\n')
print(json.dumps({'rows': len(values), 'sum': sum(values)}))
