#!/usr/bin/env python3
"""ADLS SQL differential using an independently authenticated REST fixture."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import subprocess
import urllib.error
import urllib.request

from dfs_fixture import DfsFixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['cli', 'oracle', 'extension', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--engine', choices=['cpp', 'rust', 'both'], default='both')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    report = {}
    env = {k: v for k, v in os.environ.items() if not k.startswith(('AZURE_', 'AWS_'))
           and k.lower() not in ['http_proxy', 'https_proxy', 'all_proxy', 'no_proxy']}
    env['TMPDIR'] = str(out)
    for engine, cli in [('cpp', args.oracle), ('rust', args.cli)]:
        if args.engine not in ['both', engine]:
            continue
        rows = []
        with DfsFixture() as fixture:
            try:
                urllib.request.urlopen(fixture.endpoint + '/fixture/input.csv').read()
                raise AssertionError('Fixture accepted missing authentication')
            except urllib.error.HTTPError as error:
                assert error.code == 403
            fixture.requests.clear()
            for scheme in ['abfss', 'abfs']:
                prefix = f'{scheme}://fixture'
                startup = 'SET allow_persistent_secrets=false; '
                startup += f"LOAD '{str(args.extension.resolve()).replace(chr(39), chr(39)*2)}'; " if engine == 'cpp' else 'LOAD azure; '
                startup += f"CREATE SECRET s(TYPE azure, CONNECTION_STRING '{fixture.connection}'); "
                cases = [
                    ('read', f"SELECT sum(value) FROM '{prefix}/input.csv';", ['true', '6'], False),
                    ('glob', f"SELECT file FROM glob('{prefix}/folder/*/*.csv') ORDER BY file;",
                     ['true', prefix + '/folder/a/one.csv', prefix + '/folder/b/two.csv'], False),
                    ('recursive', f"SELECT file FROM glob('{prefix}/folder/**') ORDER BY file;",
                     ['true', prefix + '/folder/a/one.csv', prefix + '/folder/b/two.csv'], False),
                    ('nested-recursive', f"SELECT file FROM glob('{prefix}/folder/**/one.csv');", ['true'], True),
                    ('staged-write', f"SET azure_write_block_size=1024; SET azure_write_staged_blocks_per_commit=1; COPY (SELECT repeat('x',300) payload FROM range(10)) TO '{prefix}/{scheme}-write.csv'; SELECT count(*), sum(length(payload)) FROM '{prefix}/{scheme}-write.csv';", ['true', '10,3000'], False),
                    ('partitioned', f"COPY (SELECT range id, range%2 part FROM range(20)) TO '{prefix}/{scheme}-parts' (FORMAT parquet, PARTITION_BY(part)); SELECT count(*),sum(id) FROM '{prefix}/{scheme}-parts/*/*.parquet';", ['true', '20,190'], False),
                ]
                for name, sql, expected, error in cases:
                    run = subprocess.run([str(cli.resolve()), '-batch', '-bail', '-csv', '-noheader', '-init', '/dev/null', ':memory:'],
                                         input=startup + sql, text=True, capture_output=True, env=env, timeout=45)
                    passed = (run.returncode != 0) == error and run.stdout.strip().splitlines() == expected
                    if error:
                        passed = passed and 'abfss do not manage recursive lookup patterns' in run.stderr
                    rows.append(dict(case=f'{scheme}-{name}', sql=sql, exit=run.returncode, stdout=run.stdout,
                                     stderr=run.stderr, passed=passed))
            signatures_valid = bool(fixture.requests) and all(r['valid_signature'] for r in fixture.requests)
            closed = sum(r.get('closed', False) for r in fixture.requests)
            report[engine] = dict(rows=rows, signatures_valid=signatures_valid, closed_flushes=closed,
                                  methods=dict(Counter(r['method'] for r in fixture.requests)))
            (out / f'{engine}-requests.json').write_text(json.dumps(fixture.requests, indent=2) + '\n')
            assert closed > 0 or not all(row['passed'] for row in rows)
    report['sha256'] = {name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                        for name in ['cli', 'oracle', 'extension']}
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({e: dict(passed=sum(r['passed'] for r in result['rows']), total=len(result['rows']),
                             signatures_valid=result['signatures_valid'], closed_flushes=result['closed_flushes'])
                      for e, result in report.items() if e != 'sha256'}))
    if any(not result['signatures_valid'] or any(not row['passed'] for row in result['rows'])
           for e, result in report.items() if e != 'sha256'):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
