#!/usr/bin/env python3
"""Run unmodified AWS S3 data-plane originals with signed local S3 fixtures."""
import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess

from s3_fixture import SignedS3Fixture

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('local_services', ROOT / 'scripts/local-services/check.py')
services = importlib.util.module_from_spec(spec)
spec.loader.exec_module(services)
FILES = ['test/sql/aws_minio.test', 'test/sql/aws_minio_secret.test', 'test/sql/aws_secret_chains_minio.test']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['slt', 'shim', 'source', 'extensions', 'tools', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    report = {}
    manifest = json.loads((ROOT / 'extensions/aws/upstream.json').read_text())
    for engine in ['cpp', 'rust']:
        # Fresh storage per engine avoids passing against the oracle's writes.
        out = args.out / engine
        with services.Services(args.tools, out) as fixture:
            fixture.s3.create_bucket(Bucket='test-bucket')
            (out / 'aws-config').write_text('''[default]
region=eu-west-1
[profile minio-testing-2]
region=eu-west-1
[profile minio-testing-invalid]
region=the-moon-123
''')
            (out / 'aws-credentials').write_text('''[default]
aws_access_key_id=minio_duckdb_user
aws_secret_access_key=minio_duckdb_user_password
[minio-testing-2]
aws_access_key_id=minio_duckdb_user_2
aws_secret_access_key=minio_duckdb_user_2_password
[minio-testing-invalid]
aws_access_key_id=minio_duckdb_user_invalid
aws_secret_access_key=thispasswordiscompletelywrong
aws_session_token=completelybogussessiontoken
''')
            reference = out / 'reference'
            for name in FILES:
                data = (args.source / name).read_bytes()
                assert hashlib.sha256(data).hexdigest() == manifest['sql_tests'][name]
                target = reference / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            keys = {'minio_duckdb_user': ('minio_duckdb_user_password', ''),
                    'minio_duckdb_user_2': ('minio_duckdb_user_2_password', '')}
            with SignedS3Fixture(fixture.s3_endpoint, keys, 'eu-west-1') as s3:
                env = {k: v for k, v in os.environ.items()
                       if not k.startswith(('AWS_', 'DUCKDB_S3_')) and k != 'LOCAL_EXTENSION_REPO'}
                env.update(AWS_CONFIG_FILE=str(out / 'aws-config'), AWS_SHARED_CREDENTIALS_FILE=str(out / 'aws-credentials'),
                           AWS_PROFILE='default', AWS_EC2_METADATA_DISABLED='true', S3_TEST_SERVER_AVAILABLE='1',
                           DUCKDB_S3_ENDPOINT=s3.endpoint, DUCKDB_S3_USE_SSL='false',
                           SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),
                           SLT_HTTPFS_CLIENT_IMPLEMENTATION='httplib', SLT_S3_URL_STYLE='path',
                           TMPDIR=str(args.tools.resolve() / 'tmp'))
                command = [str(args.slt.resolve()), 'run', '--engine', engine, '--root', str(reference),
                           '--jobs', '1', '--timeout-secs', '120', '--require', 'aws', '--require', 'httpfs',
                           '--out', str(out / 'results.jsonl'), '--log-dir', str(out / 'logs'), *FILES]
                if engine == 'cpp':
                    command += ['--shim', str(args.shim.resolve())]
                with (out / 'run.log').open('w') as log:
                    run = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
                (out / 'requests.json').write_text(json.dumps(s3.requests, indent=2) + '\n')
                rows = [json.loads(line) for line in (out / 'results.jsonl').read_text().splitlines()]
                report[engine] = dict(exit=run.returncode, counts=dict(Counter(row['status'] for row in rows)),
                                      nonpasses=[r for r in rows if r['status'] != 'pass'],
                                      auth_responses=dict(Counter(str(r['auth_status']) for r in s3.requests)))
    report['scope'] = 'Three unchanged S3 originals; SigV4, malformed region and invalid credentials enforced locally'
    (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    if any(report[e]['nonpasses'] for e in ['cpp', 'rust']):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
