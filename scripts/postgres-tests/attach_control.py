#!/usr/bin/env python3
"""Paired attachment/query/DDL/COPY checks on separately restored owned databases."""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
import subprocess

from cluster import Cluster, DATABASE
from codec_control import literal

CASES = [
    ('read', "SELECT count(*),sum(id) FROM p.source; USE p; SELECT s FROM source WHERE id=2", None),
    ('nonfirst-column', "SELECT s FROM p.source ORDER BY s LIMIT 3; SELECT s FROM postgres_query('p','SELECT 1 id,''label''::text s')", None),
    ('query', "SET pg_use_text_protocol=true; SELECT * FROM postgres_query('p','SELECT 42::integer n, current_setting(''transaction_isolation'') isolation'); SELECT * FROM postgres_query('p','SELECT $1::smallint n',params:={x:-2::SMALLINT})", None),
    ('text-fields', "SET pg_use_text_protocol=true; SELECT matrix,s FROM p.source WHERE id=1; SELECT * FROM postgres_query('p','SELECT true b, 1.25::numeric(6,2) d, date ''2020-01-01'' AS day_value, decode(''00ff'',''hex'') AS blob_value, ARRAY[1,NULL,3] a'); SELECT * FROM postgres_query('p','SELECT $1::boolean',params:={x:'f'})", None),
    ('unsupported-parameter', "SET pg_use_text_protocol=true; SELECT * FROM postgres_query('p','SELECT $1::boolean',params:={x:true})", 'Unsupported parameter type: BOOLEAN'),
    ('binary-parameters', "SELECT * FROM postgres_query('p','SELECT $1::integer',params:={x:42::INTEGER})", 'there is no parameter $1'),
    ('create-insert', "CREATE TABLE p.created(id INTEGER,s VARCHAR); INSERT INTO p.created VALUES (1,'one'),(2,'two'); SELECT * FROM p.created ORDER BY id", None),
    ('rollback-insert', "BEGIN; INSERT INTO p.source VALUES (100,'new',[7,8]); SELECT count(*) FROM p.source; ROLLBACK; SELECT count(*) FROM p.source", None),
    ('rollback-create', "BEGIN; CREATE TABLE p.aborted(i INTEGER); INSERT INTO p.aborted VALUES (1); ROLLBACK; SELECT * FROM postgres_query('p','SELECT count(*) FROM pg_tables WHERE schemaname=''public'' AND tablename=''aborted''')", None),
    ('defaults', "CREATE TABLE p.defaults(id INTEGER,s VARCHAR DEFAULT 'default',n INTEGER DEFAULT 7); INSERT INTO p.defaults(id) VALUES (1),(2); SELECT * FROM p.defaults ORDER BY id", None),
    ('ctas', "CREATE TABLE p.generated AS SELECT i::INTEGER id,'row-'||i::VARCHAR s FROM range(5000) t(i); SELECT count(*),max(id) FROM p.generated", None),
    ('replace-self-ctas', 'CREATE OR REPLACE TABLE p.source AS SELECT * FROM p.source; SELECT count(*) FROM p.source', None),
    ('existing-ctas', 'CREATE TABLE IF NOT EXISTS p.source AS SELECT 42 i; SELECT count(*) FROM p.source', None),
    ('self-insert', "BEGIN; INSERT INTO p.source SELECT id+100,s,matrix FROM p.source; SELECT count(*) FROM p.source; ROLLBACK; SELECT count(*) FROM p.source", None),
    ('multiple-streams', "SELECT a.id FROM postgres_query('p','SELECT i AS id FROM generate_series(1,20000) i') a JOIN postgres_query('p','SELECT i AS id FROM generate_series(1,20000) i') b USING(id) WHERE a.id>19995 ORDER BY a.id", None),
    ('schema', "CREATE SCHEMA p.created_schema; CREATE TABLE p.created_schema.t(i INTEGER); INSERT INTO p.created_schema.t VALUES(7); SELECT * FROM p.created_schema.t; DROP TABLE p.created_schema.t; DROP SCHEMA p.created_schema", None),
    ('enum', "CREATE TYPE p.mood AS ENUM ('sad','ok','happy'); CREATE TABLE p.moods(m p.mood); INSERT INTO p.moods VALUES ('happy'),('sad'); SELECT * FROM p.moods ORDER BY m", None),
    ('cache-clear', "SELECT count(*) FROM p.source; CALL postgres_execute('p','ALTER TABLE public.source ADD COLUMN n INTEGER DEFAULT 7'); CALL pg_clear_cache(); SELECT sum(n) FROM p.source", None),
    ('no-transaction', "CALL postgres_execute('p','VACUUM public.source',use_transaction=false); SELECT * FROM postgres_query('p','SELECT 42::integer',use_transaction=false)", None),
    ('schema-filter', "ATTACH 'dbname=postgresscanner' AS filtered(TYPE postgres,SCHEMA 'extra'); USE filtered; SELECT * FROM extra_table", None),
    ('isolation', "ATTACH 'dbname=postgresscanner' AS committed(TYPE postgres,ISOLATION_LEVEL 'READ COMMITTED'); SELECT * FROM postgres_query('committed','SELECT current_setting(''transaction_isolation'')')", None),
    ('read-only', "ATTACH 'dbname=postgresscanner' AS r(TYPE postgres,READ_ONLY); CALL postgres_execute('r','INSERT INTO public.source VALUES(99,''x'',NULL)')", 'read-only'),
    ('external-disabled', "SET enable_external_access=false; ATTACH 'dbname=postgresscanner' AS forbidden(TYPE postgres)", 'Attaching Postgres databases is disabled through configuration'),
]

for binary in ['true', 'false']:
    CASES.append(('copy-'+binary, f"SET pg_use_binary_copy={binary}; CREATE TABLE p.copies(i INTEGER,d DECIMAL(12,3),f FLOAT,b BLOB,a INTEGER[]); INSERT INTO p.copies VALUES(7,12.345,1.25,'\\x00\\xFF'::BLOB,[1,NULL,3]),(NULL,NULL,NULL,NULL,NULL); SELECT * FROM p.copies ORDER BY i NULLS LAST", None))
    CASES.append(('null-replacement-'+binary, f"SET pg_use_binary_copy={binary}; SET pg_null_byte_replacement='NULLBYTE'; CREATE TABLE p.nul(s VARCHAR); INSERT INTO p.nul VALUES('a'||chr(0)||'b'),(''); SELECT * FROM p.nul ORDER BY s", None))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['cli', 'oracle', 'extension', 'out']:
        parser.add_argument('--'+name, required=True, type=Path)
    parser.add_argument('--case', action='append')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    cases = [case for case in CASES if not args.case or case[0] in args.case]
    if args.case and {c[0] for c in cases} != set(args.case):
        parser.error('unknown case')
    results = []
    with Cluster(out/'cluster') as cluster:
        for name, sql, error in cases:
            runs = {}
            for engine in ['cpp', 'rust']:
                cluster.reset()
                cluster.sql("CREATE TABLE source(id INTEGER PRIMARY KEY,s TEXT,matrix INTEGER[]); INSERT INTO source SELECT i,'row-'||i,ARRAY[i,NULL] FROM generate_series(1,20) i; CREATE SCHEMA extra; CREATE TABLE extra.extra_table(i INTEGER); INSERT INTO extra.extra_table VALUES(7);", database=DATABASE)
                load = f'LOAD {literal(str(args.extension.resolve()))};' if engine == 'cpp' else 'LOAD postgres;'
                case_sql = sql(engine, out) if callable(sql) else sql
                secrets=out/'secrets'/name/engine;secrets.mkdir(parents=True)
                full_sql = 'SET secret_directory='+literal(str(secrets))+'; '+load + " ATTACH 'dbname=postgresscanner' AS p(TYPE postgres); " + case_sql + ';'
                (out/(name+'.'+engine+'.sql')).write_text(full_sql)
                binary = args.oracle if engine == 'cpp' else args.cli
                run = subprocess.run([str(binary.resolve()), '-batch', '-bail', '-csv', '-noheader', '-nullvalue', '__NULL__', '-init', '/dev/null', ':memory:'],
                                     input=full_sql, env=cluster.environment, text=True, capture_output=True, timeout=60)
                (out/(name+'.'+engine+'.stdout')).write_text(run.stdout)
                (out/(name+'.'+engine+'.stderr')).write_text(run.stderr)
                runs[engine] = run
            cpp, rust = runs['cpp'], runs['rust']
            if error is None:
                passed = cpp.returncode == rust.returncode == 0 and list(csv.reader(io.StringIO(cpp.stdout))) == list(csv.reader(io.StringIO(rust.stdout)))
            else:
                passed = all(r.returncode != 0 and error.lower() in r.stderr.lower() for r in runs.values())
            results.append(dict(name=name,cpp_exit=cpp.returncode,rust_exit=rust.returncode,passed=passed))
            print(json.dumps(results[-1]),flush=True)
            (out/'progress.json').write_text(json.dumps(results,indent=2)+'\n')
    report = dict(cases=results,server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],
                  sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['cli','oracle','extension']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not report['server_stopped'] or not all(r['passed'] for r in results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
