"""Native relation joins/composition and write journeys against frozen C++ outcomes."""
import gc
import json
from pathlib import Path
import sys
import duckdb
import pandas as pd

outcomes = {}


def capture(name, action):
    try:
        outcomes[name] = {'value': action()}
    except Exception as error:
        outcomes[name] = {'error': type(error).__name__, 'message': str(error)}


with duckdb.connect() as con:
    left = con.sql("SELECT * FROM (VALUES (1,'a'),(2,'b'),(NULL,'n')) t(i,l)", alias='lhs')
    right = con.sql("SELECT * FROM (VALUES (2,'x'),(3,'y'),(NULL,'m')) t(i,r)", alias='rhs')
    for how in ['inner', 'left', 'right', 'outer', 'semi', 'anti', ' LEFT ']:
        capture('using_' + how, lambda how=how: left.join(right, 'i', how).order('ALL NULLS LAST').fetchall())
    capture('using_list', lambda: left.join(right, ['i']).fetchall())
    capture('predicate', lambda: left.join(right, 'lhs.i = rhs.i').project('lhs.i AS a, rhs.r').fetchall())
    capture('filtered_projection', lambda: left.join(right, 'lhs.i = rhs.i', 'outer').filter('lhs.i IS NOT NULL').filter('lhs.i > 0').project('lhs.i, rhs.r').order('ALL NULLS LAST').limit(3).fetchall())
    capture('ordered_qualifier_hidden', lambda: left.join(right, 'i').set_alias('joined').order('i').project('lhs.i').fetchall())
    rel = con.sql('SELECT 42 AS i')
    rel = con.sql('SELECT i + 1 AS i FROM rel')
    capture('relation_variable', lambda: con.sql('SELECT * FROM rel').fetchall())
    capture('register_relation', lambda: [con.register('registered_relation', rel) is con, con.sql('SELECT * FROM registered_relation').fetchall()])
    with duckdb.connect() as target:
        capture('register_other_connection', lambda: target.register('foreign_relation', rel))
    capture('join_aggregate', lambda: left.join(right, 'lhs.i = rhs.i', 'left').aggregate('lhs.i, count(rhs.r) AS n').order('ALL NULLS LAST').fetchall())
    capture('cross_qualified', lambda: left.cross(right).project('lhs.l, rhs.r').order('ALL').fetchall())
    capture('same_alias', lambda: left.join(left, 'i'))
    capture('bad_how', lambda: left.join(right, 'i', 'full'))
    capture('empty_using', lambda: left.join(right, []))
    capture('invalid_using_element', lambda: left.join(right, ['i', 1]))
    capture('qualified_using', lambda: left.join(right, 'lhs.i'))
    capture('mixed_using', lambda: left.join(right, 'i, 1=1'))
    capture('project_aggregate_rejected', lambda: left.project('sum(i)').fetchall())
    capture('limit_negative', lambda: left.limit(-1).fetchall())
    capture('offset_negative', lambda: left.limit(2, -1).fetchall())
    with duckdb.connect() as other:
        capture('different_connections', lambda: left.join(other.sql('SELECT 1 AS i', alias='other'), 'i'))
    source = con.from_df(pd.DataFrame({'i': [1, 2, 3], 's': ['a', None, '雪']}))
    con.execute('CREATE SCHEMA s')
    for name in ['created', 's.created', '"literal.dot"', 's."quoted table"']:
        def write(name=name):
            created = source.create(name)
            initial = con.table(name).order('i').fetchall()
            inserted = source.insert_into(name)
            final = con.table(name).order('ALL NULLS LAST').fetchall()
            return [created, initial, inserted, final]
        capture('write_' + name, write)
    capture('create_duplicate', lambda: source.create('created'))
    capture('insert_missing', lambda: source.insert_into('does_not_exist'))
    con.execute('CREATE TABLE bad(i INTEGER)')
    capture('insert_mismatch', lambda: source.insert_into('bad'))
    con.execute('CREATE TABLE atomic(i INTEGER PRIMARY KEY, s VARCHAR)')
    con.execute("INSERT INTO atomic VALUES (2,'existing')")
    capture('insert_constraint', lambda: source.insert_into('atomic'))
    capture('insert_rollback', lambda: con.table('atomic').fetchall())


def retained_input_join():
    con = duckdb.connect()
    left = con.from_df(pd.DataFrame({'i': [1, 2, 3], 'l': ['a','b','c']})).set_alias('left')
    right = con.from_df(pd.DataFrame({'i': [2, 3], 'r': ['x','y']})).set_alias('right')
    result = left.join(right, 'i').project('i, l, r')
    del left, right, con
    gc.collect()
    return result.order('i').fetchall()


capture('retained_input_join', retained_input_join)
Path(sys.argv[1]).write_text(json.dumps({'version': duckdb.__version__, 'outcomes': outcomes}, indent=2) + '\n')
print(json.dumps({'outcomes': len(outcomes)}))
