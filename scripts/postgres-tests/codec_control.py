#!/usr/bin/env python3
"""Compare Rust binary codecs with C++1.5.6 over an owned real PostgreSQL server."""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
import subprocess

from cluster import Cluster, DATABASE


CASES = [
    ('numbers', "SELECT true b, '-32768'::int2 s, '-2147483648'::int4 i, '-9223372036854775808'::int8 n, 4294967295::oid o, 42.01::real f, -42.01::double precision d", []),
    ('bytes', "SELECT 'x  '::char(4) c, E'x\\t '::char(4) tab, 'héllo 🦆'::text t, decode('0001ff5c','hex') b, '{\"x\":42}'::json j, '{\"x\":42}'::jsonb jb, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid u", []),
    ('dates', "SELECT '0001-01-01 BC'::date a, '1969-12-31'::date b, 'infinity'::date c, '-infinity'::date d, '24:00:00'::time e, '12:45:01.123456+05:45'::timetz f, '1969-12-31 23:59:59.999999'::timestamp g, '2021-03-01 12:45:01+05:45'::timestamptz h, '-infinity'::timestamp i, '2 months -3 days 04:05:06.123456'::interval j", []),
    ('decimals', "SELECT 99999999999999999999999999999999999999::numeric(38,0) a, -0.12345678901234567890123456789012345678::numeric(38,38) b, 0.000000000001::numeric(18,12) c, -12345.67::numeric(10,2) d, 'NaN'::numeric(12,3) e, 0::numeric(4,2) f", []),
    ('unbounded', "SELECT 123456789.123456789::numeric a, 0.000000000000000000001::numeric b, -123.45::numeric c, 'NaN'::numeric d, 'Infinity'::numeric e, '-Infinity'::numeric f, 1234567890123456789012345678901234567890::numeric(40,0) g", []),
    ('numeric-text', "SELECT 123456789.123456789::numeric a, 'NaN'::numeric b, 1234567890123456789012345678901234567890::numeric(40,0) c", ['--numeric-as-varchar']),
    ('arrays', "SELECT ARRAY[1,NULL,-2]::integer[] a, ARRAY[]::text[] b, ARRAY['é','a,b',NULL]::text[] c, ARRAY[1.25,-3.5]::numeric(8,2)[] d, '[0:1]={7,8}'::integer[] e", []),
    ('array-fallback', "SELECT ARRAY['[1,3)'::int4range,NULL] a, ARRAY['1.2.3.4'::inet] b", []),
    ('array-text', "SELECT '{{1,NULL},{-2,3}}'::integer[] a, ARRAY['é','a,b',NULL]::text[] b", ['--array-as-varchar']),
    ('geometric', "SELECT '(1.25,-2.5)'::point a, '{1,2,3}'::line b, '[(1,2),(3,4)]'::lseg c, '(1,2),(3,4)'::box d, '[(1,2),(3,4)]'::path e, '((1,2),(3,4),(5,6))'::polygon f, '<(1,2),3>'::circle g", []),
    ('fallback', "SELECT '192.168.1.1/24'::inet a, '0101'::bit(4) b, '[1,9)'::int4range c, 'aa:bb:cc:dd:ee:ff'::macaddr d", []),
    ('custom', "SELECT 'happy'::mood a, ROW(42,'héllo','sad'::mood)::pair b, ARRAY['happy','sad']::mood[] c", []),
    ('nulls', "SELECT NULL::integer a, NULL::numeric(38,2) b, NULL::integer[] c, NULL::pair d", []),
]


def literal(value):
    return "'" + value.replace("'", "''") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['probe', 'oracle', 'extension', 'out']:
        parser.add_argument('--'+name, required=True, type=Path)
    parser.add_argument('--cli', type=Path)
    parser.add_argument('--runtime-all', action='store_true', help='Use the Rust SQL runtime for attached-table and postgres_query cases too')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []
    with Cluster(out/'cluster') as cluster:
        cluster.reset()
        cluster.sql("CREATE TYPE mood AS ENUM ('sad','ok','happy'); CREATE TYPE pair AS (number integer, label text, feeling mood);", database=DATABASE)
        cases = [(mode + '-' + name, query, flags + ([] if mode == 'table' else ['--' + mode]))
                 for mode in ['table', 'query', 'legacy'] for name, query, flags in CASES]
        for name, query, flags in cases:
            cluster.sql("DROP VIEW IF EXISTS codec_case; CREATE VIEW codec_case AS " + query, database=DATABASE)
            sql_file = out/(name+'.sql')
            sql_file.write_text(query)
            secrets=out/'secrets'/name;secrets.mkdir(parents=True)
            startup = "SET secret_directory="+literal(str(secrets))+"; "
            setup = "SET TimeZone='UTC';"
            if '--numeric-as-varchar' in flags:
                setup += ' SET pg_numeric_as_varchar=true;'
            if '--array-as-varchar' in flags:
                setup += ' SET pg_array_as_varchar=true;'
            if '--query' in flags:
                statement = f"SELECT * FROM postgres_query('p',{literal(query)})"
            elif '--legacy' in flags:
                statement = "SELECT * FROM postgres_scan('dbname=postgresscanner','public','codec_case')"
            else:
                statement = "SELECT * FROM p.public.codec_case"
            sql = f"{startup}LOAD {literal(str(args.extension.resolve()))}; {setup} ATTACH 'dbname=postgresscanner' AS p(TYPE postgres); {statement};"
            cpp = subprocess.run([str(args.oracle.resolve()), '-batch', '-bail', '-csv', '-noheader', '-nullvalue', '__NULL__', '-init', '/dev/null', ':memory:'],
                                 input=sql, env=cluster.environment, text=True, capture_output=True, timeout=40)
            if args.cli and ('--legacy' in flags or args.runtime_all):
                attachment = "" if '--legacy' in flags else "ATTACH 'dbname=postgresscanner' AS p(TYPE postgres);"
                rust_sql = f"{startup}LOAD postgres; {setup} {attachment} {statement};"
                rust = subprocess.run([str(args.cli.resolve()), '-batch', '-bail', '-csv', '-noheader', '-nullvalue', '__NULL__', '-init', '/dev/null', ':memory:'],
                                      input=rust_sql, env=cluster.environment, text=True, capture_output=True, timeout=40)
            else:
                rust = subprocess.run([str(args.probe.resolve()), str(sql_file), *flags], env=cluster.environment,
                                      text=True, capture_output=True, timeout=40)
            describe_sql = f"{startup}LOAD {literal(str(args.extension.resolve()))}; {setup} ATTACH 'dbname=postgresscanner' AS p(TYPE postgres); DESCRIBE {statement};"
            cpp_types = subprocess.run([str(args.oracle.resolve()), '-batch', '-bail', '-csv', '-noheader', '-init', '/dev/null', ':memory:'],
                                       input=describe_sql, env=cluster.environment, text=True, capture_output=True, timeout=40)
            if args.cli and ('--legacy' in flags or args.runtime_all):
                rust_types = subprocess.run([str(args.cli.resolve()), '-batch', '-bail', '-csv', '-noheader', '-init', '/dev/null', ':memory:'],
                                            input=f"{startup}LOAD postgres; {setup} {attachment} DESCRIBE {statement};", env=cluster.environment, text=True, capture_output=True, timeout=40)
                actual_types = [row[1] for row in csv.reader(io.StringIO(rust_types.stdout))]
            else:
                rust_types = subprocess.run([str(args.probe.resolve()), str(sql_file), '--types', *flags], env=cluster.environment,
                                            text=True, capture_output=True, timeout=40)
                actual_types = next(csv.reader(io.StringIO(rust_types.stdout)), [])
            expected_types = [row[1] for row in csv.reader(io.StringIO(cpp_types.stdout))]
            types_equal = cpp_types.returncode == rust_types.returncode == 0 and actual_types == expected_types
            for engine, run in [('rust', rust), ('cpp', cpp), ('rust-types', rust_types), ('cpp-types', cpp_types)]:
                (out/(name+'.'+engine+'.stdout')).write_text(run.stdout)
                (out/(name+'.'+engine+'.stderr')).write_text(run.stderr)
            equal = list(csv.reader(io.StringIO(rust.stdout))) == list(csv.reader(io.StringIO(cpp.stdout)))
            results.append(dict(name=name, rust_exit=rust.returncode, cpp_exit=cpp.returncode, equal=equal, types_equal=types_equal,
                                passed=rust.returncode == cpp.returncode == 0 and equal and types_equal))
            print(json.dumps(results[-1]), flush=True)
    report = dict(runtime_cases=len(results) if args.runtime_all else (len(CASES) if args.cli else 0), cases=results, server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],
                  sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest() for name in ['probe','oracle','extension'] + (['cli'] if args.cli else [])})
    (out/'verification.json').write_text(json.dumps(report, indent=2)+'\n')
    if not report['server_stopped'] or not all(r['passed'] for r in results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
