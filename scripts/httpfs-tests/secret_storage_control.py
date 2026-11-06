"""Persistent-secret cross-engine controls, with synthetic fixture credentials only.

The caller must authenticate engine/package before and after execution. The
secret directory must be an isolated owned test directory, never the user's
normal secret directory. All outputs are private test evidence.
"""
import argparse
import importlib.util
import json
import stat
import hashlib
import hmac
import http.server
import re
import threading
import urllib.parse
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['library', 'package', 'secrets', 'out']:
        parser.add_argument(name, type=Path)
    parser.add_argument('--mode', choices=['create', 'read', 'replace', 'drop', 'rewrite'], required=True)
    parser.add_argument('--without-extension', action='store_true')
    parser.add_argument('--consume', action='store_true')
    parser.add_argument('--expected-count', type=int, choices=[0, 8], default=8)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('s3_version_control', Path(__file__).with_name('s3_version_control.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    api = module.API(args.library, args.secrets)
    records = []
    def query(label, sql):
        record = {'label': label, **api.query(sql)}
        records.append(record)
        (args.out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        assert record['status'] == 0, record
        return record['rows']
    try:
        if not args.without_extension:
            query('load', f"LOAD '{module.quote(args.package / 'httpfs.duckdb_extension')}'")
        if args.mode in ['create', 'rewrite']:
            statements = [
                "CREATE PERSISTENT SECRET http_empty (TYPE http)",
                "CREATE PERSISTENT SECRET http_all (TYPE http, BEARER_TOKEN 'fixture-http', HTTP_PROXY 'http://fixture.invalid:8080', HTTP_PROXY_USERNAME 'fixture-user', HTTP_PROXY_PASSWORD 'fixture-pass', VERIFY_SSL true, EXTRA_HTTP_HEADERS MAP {'X-Second':'two','X-First':'one'}, SCOPE ['https://fixture.invalid/a','https://fixture.invalid/b'])",
                "CREATE PERSISTENT SECRET s3_all (TYPE s3, KEY_ID 'fixture-access', SECRET 'fixture-secret', SESSION_TOKEN 'fixture-session', REGION 'fixture-region', ENDPOINT 'fixture.invalid', URL_STYLE 'path', USE_SSL false, REQUESTER_PAYS true, KMS_KEY_ID 'fixture-kms', SCOPE ['s3://bucket/a/','s3://bucket/b/'])",
                "CREATE PERSISTENT SECRET aws_all (TYPE aws, KEY_ID 'fixture-access', SECRET 'fixture-secret', USE_SSL true)",
                "CREATE PERSISTENT SECRET r2_all (TYPE r2, KEY_ID 'fixture-r2', SECRET 'fixture-r2-secret', ACCOUNT_ID 'fixture-account')",
                "CREATE PERSISTENT SECRET gcs_all (TYPE gcs, BEARER_TOKEN 'fixture-gcs', SCOPE 'gs://fixture/')",
                "CREATE PERSISTENT SECRET hf_all (TYPE huggingface, TOKEN 'fixture-hf', SCOPE 'hf://datasets/fixture/')",
                "CREATE PERSISTENT SECRET \"MiXeD\" (TYPE http, BEARER_TOKEN 'fixture-mixed', SCOPE [''])",
            ]
            for index, sql in enumerate(statements):
                if args.mode == 'rewrite':
                    sql = sql.replace('CREATE PERSISTENT', 'CREATE OR REPLACE PERSISTENT')
                query('create-' + str(index), sql)
        if args.mode == 'replace':
            query('replace', "CREATE OR REPLACE PERSISTENT SECRET http_all (TYPE http, BEARER_TOKEN 'fixture-replacement', EXTRA_HTTP_HEADERS MAP {'X-New':'replacement'}, SCOPE ['https://replacement.invalid/'])")
        query('secrets', 'SELECT name,type,provider,persistent,storage,scope,secret_string FROM duckdb_secrets() ORDER BY name')
        query('count', 'SELECT count(*) FROM duckdb_secrets()')
        if args.consume:
            assert not args.without_extension
            seen = []
            class Handler(http.server.BaseHTTPRequestHandler):
                def log_message(self, *unused):
                    pass

                def respond(self):
                    match = re.fullmatch(r'AWS4-HMAC-SHA256 Credential=fixture-access/([^/]+)/fixture-region/s3/aws4_request, SignedHeaders=([^,]+), Signature=([0-9a-f]{64})', self.headers.get('Authorization', ''))
                    valid = False
                    if match:
                        date, signed, signature = match.groups()
                        names = signed.split(';')
                        digest = hashlib.sha256(b'').hexdigest()
                        parsed = urllib.parse.urlsplit(self.path)
                        headers = ''.join(name + ':' + ' '.join(self.headers.get(name, '').split()) + '\n' for name in names)
                        canonical = '\n'.join([self.command, parsed.path, parsed.query, headers, signed, digest])
                        scope = date + '/fixture-region/s3/aws4_request'
                        message = '\n'.join(['AWS4-HMAC-SHA256', self.headers.get('x-amz-date', ''), scope, hashlib.sha256(canonical.encode()).hexdigest()])
                        key = b'AWS4fixture-secret'
                        for value in [date, 'fixture-region', 's3', 'aws4_request']:
                            key = hmac.new(key, value.encode(), hashlib.sha256).digest()
                        valid = (hmac.compare_digest(signature, hmac.new(key, message.encode(), hashlib.sha256).hexdigest())
                                 and names == sorted(names) and self.headers.get('x-amz-content-sha256') == digest
                                 and self.headers.get('x-amz-security-token') == 'fixture-session'
                                 and self.headers.get('x-amz-request-payer') == 'requester'
                                 and parsed.path == '/bucket/a/data.csv')
                    data = b'i\n7\n'
                    status = 200 if valid else 403
                    headers = {}
                    if valid and self.command == 'GET' and self.headers.get('Range'):
                        start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                        data = data[start:end + 1]
                        headers['Content-Range'] = f'bytes {start}-{end}/4'
                        status = 206
                    seen.append({'method': self.command, 'valid': valid, 'status': status})
                    self.send_response(status)
                    self.send_header('Content-Length', str(len(data)))
                    self.send_header('ETag', '"fixture"')
                    for name, value in headers.items():
                        self.send_header(name, value)
                    self.end_headers()
                    if self.command == 'GET':
                        self.wfile.write(data)

                do_HEAD = do_GET = respond

            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
            threading.Thread(target=server.serve_forever, daemon=True).start()
            try:
                query('no-proxy', "SET http_proxy=''")
                url = f's3://bucket/a/data.csv?s3_endpoint=127.0.0.1%3A{server.server_port}&s3_use_ssl=false&s3_url_style=path'
                assert query('consume-persisted-secret', f"SELECT sum(i) FROM read_csv('{url}')") == [['7']]
                assert seen and all(request['valid'] for request in seen), seen
            finally:
                server.shutdown()
                server.server_close()
                (args.out / 'requests.json').write_text(json.dumps(seen, indent=2) + '\n')
        if args.mode == 'drop':
            for name in ['http_empty', 'http_all', 's3_all', 'aws_all', 'r2_all', 'gcs_all', 'hf_all', 'mixed']:
                query('drop-' + name, f'DROP PERSISTENT SECRET {name}')
            assert query('after-drop', 'SELECT count(*) FROM duckdb_secrets()') == [['0']]
        else:
            assert next(record['rows'] for record in records if record['label'] == 'count') == [[str(args.expected_count)]]
            files = list(args.secrets.glob('*.duckdb_secret'))
            assert len(files) == args.expected_count, files
            assert all(stat.S_IMODE(file.stat().st_mode) == 0o600 for file in files)
            assert not list(args.secrets.glob('*.tmp-*'))
    finally:
        api.close()
        (args.out / 'records.json').write_text(json.dumps(records, indent=2) + '\n')
    print(json.dumps({'mode': args.mode, 'records': len(records), 'without_extension': args.without_extension}))


if __name__ == '__main__':
    main()
