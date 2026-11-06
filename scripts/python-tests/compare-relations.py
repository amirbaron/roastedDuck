"""Run separately with the frozen C++ oracle and the native Rust wheel; compare outcomes exactly."""
from pathlib import Path
import gc
import json
import sys
import duckdb

outcomes = {}


def encode(value):
    if isinstance(value, tuple):
        return {"tuple": [encode(item) for item in value]}
    if isinstance(value, list):
        return [encode(item) for item in value]
    if isinstance(value, dict):
        return {key: encode(item) for key, item in value.items()}
    return value


def capture(name, action):
    try:
        outcomes[name] = {"value": encode(action())}
    except Exception as error:
        outcomes[name] = {"error": type(error).__name__, "message": str(error)}


with duckdb.connect() as con:
    con.execute('CREATE TABLE "a.b"(i INTEGER); INSERT INTO "a.b" VALUES (1),(2),(2),(NULL)')
    con.execute('CREATE TABLE "semi;colon"(i INTEGER); INSERT INTO "semi;colon" VALUES (42)')
    con.execute('CREATE TABLE "a""b"(i INTEGER); INSERT INTO "a""b" VALUES (43)')
    con.execute('CREATE VIEW "v.dot" AS SELECT 44 AS i')
    for name in ['"a.b"', 'main."a.b"', 'a.b', 'semi;colon', 'a"b', '"a""b"', 'a.b.c.d', '"unclosed']:
        capture('table_' + name, lambda name=name: con.table(name).order('i NULLS LAST').fetchall())
    capture('view_literal_name', lambda: con.view('v.dot').fetchall())
    capture('relation_type', lambda: [con.table('"a.b"').type, con.view('v.dot').type])
    relation = con.table('"a.b"')
    capture('composition', lambda: relation.filter('i > 0').project('i * 2 AS j').distinct().order('j DESC').limit(1).fetchall())
    capture('grouping', lambda: relation.aggregate('i, count(*) AS n').order('i NULLS LAST').fetchall())
    capture('repeat_fetchall', lambda: [relation.fetchall(), relation.fetchall()])
    left = con.sql('SELECT * FROM (VALUES (1),(1),(2)) t(i)')
    right = con.sql('SELECT * FROM (VALUES (1),(3)) t(i)')
    for operation in ['union', 'except_', 'intersect']:
        capture(operation, lambda operation=operation: getattr(left, operation)(right).order('i').fetchall())
    with duckdb.connect() as other:
        capture('different_connections', lambda: left.union(other.sql('SELECT 1')))
    capture('bound_parameter_composition', lambda: con.sql('SELECT ?::INTEGER AS i', params=[42]).project('i + 1 AS j').fetchall())
    capture('array_output', lambda: con.sql('SELECT [1,2]::INTEGER[2], [1,2]').fetchall())
    for key in ['[1,2]', "{'a':1}", '[1,2]::INTEGER[2]']:
        capture('map_' + key, lambda key=key: con.sql(f'SELECT MAP([{key}], [42])').fetchall())


def retained_owner():
    owner = duckdb.connect()
    relation = owner.sql('SELECT 42')
    del owner
    gc.collect()
    return relation.fetchall()


def closed_owner():
    owner = duckdb.connect()
    relation = owner.sql('SELECT 42')
    owner.close()
    return relation.fetchall()


def interleaved():
    with duckdb.connect() as owner:
        first = owner.sql('SELECT * FROM range(5000)')
        initial = first.fetchone()
        second = owner.sql('SELECT 99')
        middle = second.fetchall()
        try:
            remaining = first.fetchall()
            final = [len(remaining), remaining[0], remaining[-1]]
        except Exception as error:
            final = [type(error).__name__, str(error)]
        return [initial, middle, final]


def close_relation(fetch_first=False):
    with duckdb.connect() as owner:
        relation = owner.sql('SELECT 42')
        if fetch_first:
            relation.fetchall()
        relation.close()
        return [relation.fetchone(), relation.fetchmany(), relation.fetchall()]


def metadata_preserves_stream():
    with duckdb.connect() as owner:
        relation = owner.sql('SELECT * FROM range(5000)')
        first = relation.fetchone()
        metadata = owner.sql('SELECT 99 AS answer').columns
        remaining = relation.fetchall()
        return [first, metadata, len(remaining), remaining[-1]]


def metadata_preserves_connection_result():
    with duckdb.connect() as owner:
        owner.execute('SELECT * FROM range(5000)')
        first = owner.fetchone()
        metadata = owner.sql('SELECT 99 AS answer').columns
        remaining = owner.fetchall()
        return [first, metadata, len(remaining), remaining[-1]]


def failed_metadata_preserves_stream():
    with duckdb.connect() as owner:
        relation = owner.sql('SELECT * FROM range(5000)')
        first = relation.fetchone()
        try:
            owner.sql('SELECT * FROM missing_relation_table')
        except duckdb.CatalogException:
            pass
        else:
            raise AssertionError('Missing table unexpectedly bound')
        remaining = relation.fetchall()
        return [first, len(remaining), remaining[-1]]


capture('retained_owner', retained_owner)
capture('closed_owner', closed_owner)
capture('interleaved', interleaved)
capture('close_relation', close_relation)
capture('close_after_fetchall', lambda: close_relation(True))
capture('metadata_preserves_stream', metadata_preserves_stream)
capture('metadata_preserves_connection_result', metadata_preserves_connection_result)
capture('failed_metadata_preserves_stream', failed_metadata_preserves_stream)
Path(sys.argv[1]).write_text(json.dumps({'version': duckdb.__version__, 'outcomes': outcomes}, indent=2) + '\n')
print(json.dumps({'outcomes': len(outcomes)}))
