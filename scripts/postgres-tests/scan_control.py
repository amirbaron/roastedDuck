#!/usr/bin/env python3
"""SQL scan controls, with C++ as oracle and an owned TLS/SCRAM PostgreSQL server."""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
import subprocess

from cluster import Cluster, DATABASE
from codec_control import literal


SCAN = "postgres_scan('dbname=postgresscanner','public','scan_rows')"
PUSH = "postgres_scan_pushdown('dbname=postgresscanner','public','scan_rows')"
MATRIX = "postgres_scan('dbname=postgresscanner','public','scan_matrix')"
CASES = [
    ('projection', f'SELECT label, id FROM {SCAN} WHERE id IN (1,2048,2049,5000) ORDER BY id', None),
    ('count', f'SELECT count(*) FROM {SCAN}', None),
    ('zero-column-projection', f'SELECT 42 FROM {SCAN} LIMIT 3', None),
    ('aggregate', f'SELECT count(*),sum(id),max(id),min(label) FROM {SCAN}', None),
    ('empty', "SELECT * FROM postgres_scan('dbname=postgresscanner','public','scan_empty')", None),
    ('filter', f"SELECT id FROM {PUSH} WHERE (id BETWEEN 2 AND 5 OR id=4999) AND label IS NOT NULL ORDER BY id", None),
    ('join', f'SELECT a.id,b.label FROM {SCAN} a JOIN {PUSH} b USING(id) WHERE a.id IN (2,4999) ORDER BY a.id', None),
    ('prepared-reexecution', f'PREPARE p AS SELECT count(*) FROM {SCAN}; EXECUTE p; EXECUTE p', None),
    ('matrix', f'SELECT * FROM {MATRIX} ORDER BY id', None),
    ('matrix-text', f'SET pg_array_as_varchar=true; SELECT * FROM {MATRIX} ORDER BY id', None),
    ('mixed-dimensions', "SELECT * FROM postgres_scan('dbname=postgresscanner','public','scan_mixed')", 'Expected an array with 1 dimensions, but this array has 2 dimensions'),
    ('mixed-dimensions-text', "SET pg_array_as_varchar=true; SELECT * FROM postgres_scan('dbname=postgresscanner','public','scan_mixed') ORDER BY id", None),
    ('missing-table', "SELECT * FROM postgres_scan('dbname=postgresscanner','public','missing')", ''),
    ('null-argument', "SELECT * FROM postgres_scan(NULL,'public','scan_rows')", ''),
    ('legacy-external-setting', f'SET enable_external_access=false; SELECT count(*) FROM {SCAN}', None),
    ('legacy-numeric-setting', "SET pg_numeric_nan_as_null=false; SELECT * FROM postgres_scan('dbname=postgresscanner','public','scan_nan')", None),
    ('numeric-null', "SELECT * FROM postgres_scan('dbname=postgresscanner','public','scan_nan')", None),
    ('quoted-relation', "SELECT * FROM postgres_scan('dbname=postgresscanner','public','quote'';" + '"' + "')", None),
    ('legacy-attach', "CALL postgres_attach('dbname=postgresscanner'); SELECT count(*) FROM scan_rows", None),
    ('legacy-preserve', "CREATE VIEW scan_rows AS SELECT -1 id; CALL postgres_attach('dbname=postgresscanner'); SELECT * FROM scan_rows", None),
    ('legacy-overwrite', "CREATE VIEW scan_rows AS SELECT -1 id; CALL postgres_attach('dbname=postgresscanner', overwrite=true); SELECT count(*) FROM scan_rows", None),
    ('legacy-schema-filter', "CREATE SCHEMA sink; CALL postgres_attach('dbname=postgresscanner',source_schema='scan_schema',sink_schema='sink',filter_pushdown=true); SELECT * FROM sink.custom WHERE id > 1", None),
    ('legacy-suffix-unused', "CALL postgres_attach('dbname=postgresscanner',source_schema='scan_schema',suffix='_ignored'); SELECT * FROM custom ORDER BY id", None),
    ('legacy-missing-sink', "CALL postgres_attach('dbname=postgresscanner',source_schema='scan_schema',sink_schema='missing'); SELECT count(*) FROM information_schema.tables WHERE table_name='custom'", None),
    ('autoload', f'SELECT count(*) FROM {SCAN}', None),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['cli', 'oracle', 'extension', 'out']:
        parser.add_argument('--'+name, required=True, type=Path)
    parser.add_argument('--interrupt-probe', type=Path)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []
    with Cluster(out/'cluster') as cluster:
        cluster.reset()
        cluster.sql('''
CREATE SCHEMA scan_schema; CREATE TABLE scan_schema.custom(id integer); INSERT INTO scan_schema.custom VALUES (1),(2);
CREATE VIEW slow_scan AS SELECT 1::integer id FROM pg_sleep(30);
CREATE TABLE scan_rows AS SELECT i::integer id, 'row-'||i::text label FROM generate_series(1,5000) i;
CREATE TABLE scan_empty (id integer, label text);
CREATE TABLE scan_matrix (id integer, matrix integer[][]);
INSERT INTO scan_matrix VALUES (1,'{{1,NULL},{-2,3}}'),(2,'{}'),(3,NULL);
CREATE TABLE scan_mixed (id integer, matrix integer[]);
INSERT INTO scan_mixed VALUES (1,'{1,2}'),(2,'{{1,2},{3,4}}');
CREATE TABLE scan_nan (number numeric(5,2)); INSERT INTO scan_nan VALUES ('NaN'),(1.25);
CREATE TABLE "quote';""" (id integer); INSERT INTO "quote';""" VALUES (7);
''', database=DATABASE)
        for name, sql, error in CASES:
            runs = {}
            for engine in ['cpp', 'rust']:
                prefix = f'LOAD {literal(str(args.extension.resolve()))};' if engine == 'cpp' else 'LOAD postgres;'
                # Rust autoload is tested without an explicit LOAD. The reference
                # extension is explicitly loaded to avoid downloads to user HOME.
                if name == 'autoload' and engine == 'rust':
                    prefix = 'SET autoload_known_extensions=true;'
                binary = args.oracle if engine == 'cpp' else args.cli
                run = subprocess.run([str(binary.resolve()), '-batch', '-bail', '-csv', '-noheader', '-nullvalue', '__NULL__', '-init', '/dev/null', ':memory:'],
                                     input=prefix + sql + ';', env=cluster.environment, text=True, capture_output=True, timeout=45)
                (out/(name+'.'+engine+'.stdout')).write_text(run.stdout)
                (out/(name+'.'+engine+'.stderr')).write_text(run.stderr)
                runs[engine] = run
            cpp, rust = runs['cpp'], runs['rust']
            if error is not None:
                passed = all(run.returncode != 0 and error in run.stderr for run in runs.values())
            else:
                passed = cpp.returncode == rust.returncode == 0 and list(csv.reader(io.StringIO(cpp.stdout))) == list(csv.reader(io.StringIO(rust.stdout)))
            result = dict(name=name, cpp_exit=cpp.returncode, rust_exit=rust.returncode, passed=passed)
            results.append(result)
            print(json.dumps(result), flush=True)
        if args.interrupt_probe:
            run = subprocess.run([str(args.interrupt_probe.resolve())], env=cluster.environment,
                                 text=True, capture_output=True, timeout=40)
            (out/'interrupt.stdout').write_text(run.stdout)
            (out/'interrupt.stderr').write_text(run.stderr)
            results.append(dict(name='rust-api-interrupt', rust_exit=run.returncode,
                                passed=run.returncode == 0 and run.stdout.strip() == 'interrupt-ok'))
            print(json.dumps(results[-1]), flush=True)
    report = dict(cases=results, server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],
                  sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest() for name in ['cli','oracle','extension'] + (['interrupt_probe'] if args.interrupt_probe else [])})
    (out/'verification.json').write_text(json.dumps(report, indent=2)+'\n')
    if not report['server_stopped'] or not all(r['passed'] for r in results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
