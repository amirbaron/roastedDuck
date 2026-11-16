#!/usr/bin/env python3
"""Run unchanged AWS environment originals with isolated fake credentials."""
import argparse
from collections import Counter
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import threading

ROOT = Path(__file__).resolve().parents[2]
FILES = [f'test/sql/env/{name}.test' for name in
         ['aws_env_var', 'aws_secret_chains_env', 'aws_secret_refresh']]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['slt', 'shim', 'source', 'extensions', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    (out / 'config').write_text('')
    (out / 'credentials').write_text('')
    manifest = json.loads((ROOT / 'extensions/aws/upstream.json').read_text())
    reference = out / 'reference'
    for name in FILES:
        data = (args.source / name).read_bytes()
        assert hashlib.sha256(data).hexdigest() == manifest['sql_tests'][name]
        target = reference / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    requests = []

    class Denied(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reject(self):
            requests.append(dict(method=self.command, path=self.path))
            self.send_response(403)
            self.send_header('Content-Length', '0')
            self.end_headers()

        do_HEAD = do_GET = do_CONNECT = reject

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Denied)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('AWS_', 'DUCKDB_S3_', 'SLT_'))
           and k.lower() not in ['http_proxy', 'https_proxy', 'all_proxy', 'no_proxy']
           and k != 'LOCAL_EXTENSION_REPO'}
    env.update(AWS_CONFIG_FILE=str(out / 'config'), AWS_SHARED_CREDENTIALS_FILE=str(out / 'credentials'),
               AWS_EC2_METADATA_DISABLED='true', AWS_ACCESS_KEY_ID='duckdb_env_testing_id',
               AWS_SECRET_ACCESS_KEY='duckdb_env_testing_key', AWS_DEFAULT_REGION='duckdb_env_testing_region',
               DUCKDB_AWS_TESTING_ENV_AVAILABLE='1', DUCKDB_S3_ENDPOINT=f'127.0.0.1:{server.server_port}',
               DUCKDB_S3_USE_SSL='false', SLT_S3_URL_STYLE='path',
               SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()), TMPDIR=str(out))
    proxy = f'http://127.0.0.1:{server.server_port}'
    env.update(HTTP_PROXY=proxy, HTTPS_PROXY=proxy, http_proxy=proxy, https_proxy=proxy, NO_PROXY='', no_proxy='')
    # Explicit normal SQL setting avoids transport-specific environment rules.
    env.update(SLT_HTTP_PROXY=proxy, SLT_HTTPFS_CLIENT_IMPLEMENTATION='httplib')
    report = {}
    try:
        for engine in ['cpp', 'rust']:
            before = len(requests)
            command = [str(args.slt.resolve()), 'run', '--engine', engine, '--root', str(reference),
                       '--jobs', '1', '--timeout-secs', '90', '--require', 'aws', '--require', 'httpfs',
                       '--out', str(out / f'{engine}.jsonl'), '--log-dir', str(out / f'{engine}-logs'), *FILES]
            if engine == 'cpp':
                command += ['--shim', str(args.shim.resolve())]
            with (out / f'{engine}.log').open('w') as log:
                run = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
            rows = [json.loads(line) for line in (out / f'{engine}.jsonl').read_text().splitlines()]
            report[engine] = dict(exit=run.returncode, counts=dict(Counter(r['status'] for r in rows)), rows=rows,
                                 requests=requests[before:])
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    report['requests'] = requests
    report['sha256'] = {name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                        for name in ['slt', 'shim']}
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    if any(r['status'] != 'pass' for engine in ['cpp', 'rust'] for r in report[engine]['rows']):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
