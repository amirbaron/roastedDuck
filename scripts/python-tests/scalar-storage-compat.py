"""Real two-engine persistence of scalar expressions, indexes and nonempty WAL."""
import argparse
import json
import os
from pathlib import Path
import duckdb

parser = argparse.ArgumentParser()
parser.add_argument('mode', choices=['checkpoint', 'wal', 'read'])
parser.add_argument('database', type=Path)
args = parser.parse_args()
con = duckdb.connect(str(args.database), config={'threads': 1})
if args.mode != 'read':
    con.execute("CREATE TABLE t(i INTEGER, j INTEGER GENERATED ALWAYS AS (abs(i)), s VARCHAR DEFAULT lower('ABC'), CHECK (i >= -1000))")
    con.execute('CREATE INDEX scalar_idx ON t((abs(i)))')
    con.execute('CHECKPOINT')
    con.execute('INSERT INTO t(i) VALUES (-7),(3),(NULL)')
    assert con.execute('SELECT i,j,s FROM t ORDER BY i NULLS LAST').fetchall() == [(-7,7,'abc'),(3,3,'abc'),(None,None,'abc')]
    if args.mode == 'checkpoint':
        con.execute('CHECKPOINT')
        con.close()
    print(json.dumps({'written': args.mode, 'database': str(args.database)}), flush=True)
    if args.mode == 'wal':
        os._exit(0)
else:
    rows = con.execute('SELECT i,j,s FROM t ORDER BY i NULLS LAST').fetchall()
    assert rows == [(-7,7,'abc'),(3,3,'abc'),(None,None,'abc')], rows
    assert con.execute('SELECT i,j FROM t WHERE abs(i)=7').fetchall() == [(-7,7)]
    assert con.execute("SELECT index_name FROM duckdb_indexes() WHERE index_name='scalar_idx'").fetchall() == [('scalar_idx',)]
    try:
        con.execute('INSERT INTO t(i) VALUES (-1001)')
        raise AssertionError('CHECK constraint accepted invalid value')
    except duckdb.ConstraintException:
        pass
    con.execute('INSERT INTO t(i) VALUES (-8)')
    assert con.execute('SELECT i,j,s FROM t WHERE i=-8').fetchall() == [(-8,8,'abc')]
    con.close()
    print(json.dumps({'read': True, 'rows': rows, 'functional_index': True,
                      'generated_column': True, 'default_function': True,
                      'constraint_and_recovery': True}))
