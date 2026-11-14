#!/usr/bin/env python3
"""Run original AWS proxy SQL with signed S3 and authenticated loopback proxies."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import subprocess

from s3_fixture import ForwardProxy, SignedS3Fixture
from upstream_data import ROOT, services

FILES = ['test/sql/env/proxy/aws_secret_http_proxy.test',
         'test/sql/env/proxy/aws_secret_http_proxy_with_auth.test']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['slt', 'shim', 'source', 'extensions', 'tools', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    manifest = json.loads((ROOT / 'extensions/aws/upstream.json').read_text())
    report = {}
    for engine in ['cpp', 'rust']:
        with services.Services(args.tools, out / engine) as fixture:
            fixture.s3.create_bucket(Bucket='test-bucket')
            reference = out / engine / 'reference'
            for name in FILES:
                data = (args.source / name).read_bytes()
                assert hashlib.sha256(data).hexdigest() == manifest['sql_tests'][name]
                target = reference / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            keys = {'fixture-access': ('fixture-secret', '')}
            report[engine] = []
            with SignedS3Fixture(fixture.s3_endpoint, keys, 'eu-west-1') as s3:
                for index, name in enumerate(FILES):
                    with ForwardProxy(s3.endpoint, reference / 'mitmproxy.log', authenticated=index == 1) as proxy:
                        env = {k: v for k, v in os.environ.items()
                               if not k.startswith(('AWS_', 'DUCKDB_S3_', 'SLT_'))
                               and k.lower() not in ['http_proxy', 'https_proxy', 'all_proxy', 'no_proxy']
                               and k != 'LOCAL_EXTENSION_REPO'}
                        env.update(AWS_CONFIG_FILE=str(out / engine / 'aws-config'),
                                   AWS_SHARED_CREDENTIALS_FILE=str(out / engine / 'aws-credentials'),
                                   AWS_ACCESS_KEY_ID='fixture-access', AWS_SECRET_ACCESS_KEY='fixture-secret',
                                   AWS_EC2_METADATA_DISABLED='true', S3_TEST_SERVER_AVAILABLE='1',
                                   DUCKDB_S3_ENDPOINT=s3.endpoint, DUCKDB_S3_USE_SSL='false',
                                   HTTP_PROXY_PUBLIC=proxy.url, NO_PROXY='', no_proxy='',
                                   SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),
                                   SLT_HTTPFS_CLIENT_IMPLEMENTATION='httplib', SLT_S3_URL_STYLE='path',
                                   TMPDIR=str(args.tools.resolve() / 'tmp'))
                        result = out / engine / f'{index}.jsonl'
                        command = [str(args.slt.resolve()), 'run', '--engine', engine, '--root', str(reference),
                                   '--jobs', '1', '--timeout-secs', '90', '--require', 'aws', '--require', 'httpfs',
                                   '--out', str(result), '--log-dir', str(out / engine / 'logs'), name]
                        if engine == 'cpp':
                            command += ['--shim', str(args.shim.resolve())]
                        with (out / engine / f'{index}.log').open('w') as log:
                            run = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
                        rows = [json.loads(line) for line in result.read_text().splitlines()]
                        report[engine].append(dict(exit=run.returncode, rows=rows, proxy_requests=proxy.requests))
                (out / engine / 's3-requests.json').write_text(json.dumps(s3.requests, indent=2) + '\n')
                assert s3.requests and all(r['valid_signature'] for r in s3.requests)
    report['sha256'] = {name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                        for name in ['slt', 'shim']}
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({e: dict(Counter(row['status'] for case in report[e] for row in case['rows']))
                      for e in ['cpp', 'rust']}))
    if any(r['status'] != 'pass' for e in ['cpp', 'rust'] for case in report[e] for r in case['rows']):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
