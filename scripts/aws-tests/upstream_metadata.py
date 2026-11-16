#!/usr/bin/env python3
"""Run the unchanged AWS metadata/profile SQL cohort with local fixtures."""
import argparse
from collections import Counter
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import threading

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('local_services', ROOT / 'scripts/local-services/check.py')
services = importlib.util.module_from_spec(spec)
spec.loader.exec_module(services)

FILES = [f'test/sql/{name}.test' for name in [
    'aws_errors', 'aws_secret', 'aws_secret_aws', 'aws_secret_chains',
    'aws_secret_external_id', 'aws_secret_gcs', 'aws_secret_r2',
    'aws_secret_redaction', 'aws_secret_region_warning', 'aws_secret_sts_provider',
    'aws_secret_validation',
]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--slt', required=True, type=Path)
    parser.add_argument('--shim', required=True, type=Path)
    parser.add_argument('--source', required=True, type=Path)
    parser.add_argument('--extensions', required=True, type=Path)
    parser.add_argument('--tools', required=True, type=Path)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--httpfs-client', choices=['default', 'curl', 'httplib'], default='default')
    args = parser.parse_args()
    args.out = args.out.resolve()
    denied = []

    class DenyIdentity(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reject(self):
            denied.append({'method': self.command, 'authority': self.headers.get('Host', self.path)})
            body = b'<ErrorResponse><Error><Code>AccessDenied</Code><Message>local unavailable role fixture</Message></Error></ErrorResponse>'
            self.send_response(403)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Connection', 'close')
            self.end_headers()
            self.wfile.write(body)

        do_CONNECT = do_GET = do_POST = reject

    report = {}
    with services.Services(args.tools, args.out) as fixture:
        # A real local S3 API is available; this cohort does not claim data-plane
        # signature acceptance (the network control provides that independently).
        fixture.s3.create_bucket(Bucket='test-bucket')
        fixture.s3.put_object(Bucket='test-bucket', Key='ready', Body=b'ready')
        assert fixture.s3.get_object(Bucket='test-bucket', Key='ready')['Body'].read() == b'ready'
        (args.out / 'aws-config').write_text('''[default]
region=eu-west-1
[profile minio-testing-2]
region=eu-west-1
[profile minio-testing-invalid]
region=eu-west-1
[profile minio-testing-empty]
region=eu-west-1
[profile assume-role-arn]
region=eu-west-1
role_arn=arn:aws:iam::123456789012:role/unavailable
[profile assume-role-arn-external-id]
region=eu-west-1
role_arn=arn:aws:iam::123456789012:role/unavailable
external_id=fixture-external-id
''')
        (args.out / 'aws-credentials').write_text('''[default]
aws_access_key_id=minio_duckdb_user
aws_secret_access_key=minio_duckdb_user_password
[minio-testing-2]
aws_access_key_id=minio_duckdb_user_2
aws_secret_access_key=minio_duckdb_user_2_password
[minio-testing-invalid]
aws_access_key_id=minio_duckdb_user
aws_secret_access_key=thispasswordiscompletelywrong
''')
        manifest = json.loads((ROOT / 'extensions/aws/upstream.json').read_text())
        reference = args.out / 'reference'
        for name, expected in manifest['sql_tests'].items():
            data = (args.source / name).read_bytes()
            assert hashlib.sha256(data).hexdigest() == expected, name
            target = reference / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), DenyIdentity)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        proxy = f'http://127.0.0.1:{server.server_port}'
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(('AWS_', 'DUCKDB_S3_')) and k != 'LOCAL_EXTENSION_REPO'}
        env.update(AWS_CONFIG_FILE=str(args.out / 'aws-config'),
                   AWS_SHARED_CREDENTIALS_FILE=str(args.out / 'aws-credentials'),
                   AWS_PROFILE='default', AWS_EC2_METADATA_DISABLED='true',
                   AWS_ENDPOINT_URL=proxy, HTTPS_PROXY=proxy, https_proxy=proxy,
                   HTTP_PROXY=proxy, http_proxy=proxy, NO_PROXY='localhost,127.0.0.1',
                   S3_TEST_SERVER_AVAILABLE='1', DUCKDB_S3_ENDPOINT=fixture.s3_endpoint.removeprefix('http://'),
                   DUCKDB_S3_USE_SSL='false',
                   SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),
                   TMPDIR=str(args.tools.resolve() / 'tmp'))
        env['SLT_HTTPFS_CLIENT_IMPLEMENTATION'] = args.httpfs_client
        try:
            for engine in ['cpp', 'rust']:
                rows = []
                exit_code = 0
                with (args.out / f'{engine}.log').open('w') as log:
                    for index, name in enumerate(FILES):
                        file_env = dict(env)
                        if name.endswith('/aws_secret_aws.test'):
                            # This original tests a refused localhost connection,
                            # not a denied proxy tunnel. Its requests stay local.
                            for key in ['HTTP_PROXY', 'HTTPS_PROXY', 'http_proxy', 'https_proxy']:
                                file_env.pop(key, None)
                        result_file = args.out / f'{engine}-{index}.jsonl'
                        command = [str(args.slt.resolve()), 'run', '--engine', engine, '--root', str(reference),
                                   '--jobs', '1', '--timeout-secs', '120', '--require', 'aws', '--require', 'httpfs',
                                   '--out', str(result_file), '--log-dir', str(args.out / f'{engine}-logs'), name]
                        if engine == 'cpp':
                            command += ['--shim', str(args.shim.resolve())]
                        run = subprocess.run(command, env=file_env, stdout=log, stderr=subprocess.STDOUT)
                        exit_code = exit_code or run.returncode
                        rows.extend(json.loads(line) for line in result_file.read_text().splitlines())
                (args.out / f'{engine}.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in rows))
                report[engine] = dict(exit=exit_code, counts=dict(Counter(r['status'] for r in rows)),
                                      nonpasses=[r for r in rows if r['status'] != 'pass'])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
            (args.out / 'denied-identity-requests.json').write_text(json.dumps(denied, indent=2) + '\n')
    report['files'] = FILES
    report['httpfs_client'] = args.httpfs_client
    report['scope'] = 'Unchanged metadata/profile originals; unavailable STS roles rejected locally. Data-plane, IRSA, RDS and proxy originals are separate.'
    (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    if any(report[engine]['nonpasses'] for engine in ['cpp', 'rust']):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
