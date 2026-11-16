#!/usr/bin/env python3
"""AWS SQL/network differential with independent SigV4 checks and expiring credentials."""
import argparse
import hashlib
import hmac
import http.client
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import threading
import urllib.parse

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('local_services', ROOT / 'scripts/local-services/check.py')
services = importlib.util.module_from_spec(spec)
spec.loader.exec_module(services)


def quote(value):
    return str(value).replace("'", "''")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', type=Path, required=True)
    parser.add_argument('--oracle', type=Path, required=True)
    parser.add_argument('--aws-extension', type=Path, required=True)
    parser.add_argument('--httpfs-extension', type=Path, required=True)
    parser.add_argument('--tools', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.out = args.out.resolve()
    requests = []
    results = []
    keys = {
        'fixture-access': ('fixture-secret', ''),
        'old-key': ('old-secret', 'old-token'),
        'new-key': ('new-secret', 'new-token'),
    }
    with services.Services(args.tools, args.out) as fixture:
        fixture.s3.create_bucket(Bucket='fixture')
        fixture.s3.put_object(Bucket='fixture', Key='input.csv', Body=b'value\n1\n2\n3\n')
        backend = urllib.parse.urlsplit(fixture.s3_endpoint)

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def authenticate(self, body):
                try:
                    algorithm, fields = self.headers['Authorization'].split(' ', 1)
                    fields = dict(part.strip().split('=', 1) for part in fields.split(','))
                    key, date, region, service, terminator = fields['Credential'].split('/')
                    secret, token = keys[key]
                    assert algorithm == 'AWS4-HMAC-SHA256' and service == 's3' and terminator == 'aws4_request'
                    assert region == 'us-east-1'
                    assert self.headers.get('x-amz-security-token', '') == token
                    assert self.headers['x-amz-date'].startswith(date)
                    parsed = urllib.parse.urlsplit(self.path)
                    uri = urllib.parse.quote(urllib.parse.unquote(parsed.path), safe='/-_.~')
                    encode = lambda s: urllib.parse.quote(s, safe='-_.~')
                    query = '&'.join(k + '=' + v for k, v in sorted(
                        (encode(k), encode(v)) for k, v in urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)))
                    signed = fields['SignedHeaders']
                    headers = ''.join(name + ':' + ' '.join(self.headers[name].split()) + '\n' for name in signed.split(';'))
                    payload_hash = hashlib.sha256(body).hexdigest()
                    assert self.headers.get('x-amz-content-sha256', payload_hash) == payload_hash
                    canonical = '\n'.join([self.command, uri, query, headers, signed, payload_hash])
                    to_sign = '\n'.join([algorithm, self.headers['x-amz-date'], '/'.join([date, region, service, terminator]),
                                         hashlib.sha256(canonical.encode()).hexdigest()])
                    signing_key = ('AWS4' + secret).encode()
                    for value in [date, region, service, terminator]:
                        signing_key = hmac.new(signing_key, value.encode(), hashlib.sha256).digest()
                    assert hmac.compare_digest(hmac.new(signing_key, to_sign.encode(), hashlib.sha256).hexdigest(), fields['Signature'])
                    return key, True
                except (AssertionError, KeyError, ValueError, AttributeError, TypeError):
                    return '<invalid>', False

            def handle_request(self):
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                key, valid = self.authenticate(body)
                requests.append(dict(engine=self.server.engine, method=self.command, path=self.path,
                                     access_key=key, valid_signature=valid, expired=key == 'old-key'))
                if not valid or key == 'old-key':
                    code = 'ExpiredToken' if valid else 'SignatureDoesNotMatch'
                    payload = f'<Error><Code>{code}</Code><Message>local credential fixture</Message></Error>'.encode()
                    self.send_response(403)
                    self.send_header('Content-Length', str(len(payload)))
                    self.send_header('x-amz-error-code', code)
                    self.end_headers()
                    if self.command != 'HEAD':
                        self.wfile.write(payload)
                    return
                connection = http.client.HTTPConnection(backend.hostname, backend.port, timeout=10)
                try:
                    headers = {k: v for k, v in self.headers.items() if k.lower() not in ['connection', 'transfer-encoding']}
                    headers['Content-Length'] = str(len(body))
                    connection.request(self.command, self.path, body=body, headers=headers)
                    response = connection.getresponse()
                    payload = response.read()
                    self.send_response(response.status)
                    for name, value in response.getheaders():
                        if name.lower() not in ['connection', 'transfer-encoding', 'server', 'date', 'content-length']:
                            self.send_header(name, value)
                    self.send_header('Content-Length', response.getheader('Content-Length', str(len(payload))))
                    self.end_headers()
                    if self.command != 'HEAD':
                        self.wfile.write(payload)
                finally:
                    connection.close()

            do_GET = do_HEAD = do_PUT = do_POST = do_DELETE = handle_request

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        server.engine = ''
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        endpoint = f'127.0.0.1:{server.server_port}'
        config = args.out / 'aws-config'
        base_config = config.read_text()
        helper = args.out / 'rotate.py'
        helper.write_text("import json,pathlib,sys\np=pathlib.Path(sys.argv[1]); n=int(p.read_text())+1 if p.exists() else 1; p.write_text(str(n))\nprefix='old' if n==1 else 'new'\nprint(json.dumps({'Version':1,'AccessKeyId':prefix+'-key','SecretAccessKey':prefix+'-secret','SessionToken':prefix+'-token'}))\n")
        try:
            for engine, cli in [('cpp', args.oracle), ('rust', args.cli)]:
                server.engine = engine
                counter = args.out / f'{engine}-counter'
                command = ' '.join(shlex.quote(str(p)) for p in [sys.executable, helper, counter])
                config.write_text(base_config + f'\n[profile rotating]\nregion=us-east-1\ncredential_process={command}\n')
                startup = 'SET allow_persistent_secrets=false;\n'
                if engine == 'cpp':
                    startup += f"LOAD '{quote(args.httpfs_extension.resolve())}'; LOAD '{quote(args.aws_extension.resolve())}';\n"
                else:
                    startup += 'LOAD httpfs; LOAD aws;\n'
                for case, profile, extra, expected in [
                    ('profile-copy', 'roastedduck-fixture', '', ['true', '6', '42']),
                    ('refresh', 'rotating', "CHAIN 'process', REFRESH 'auto',", ['true', '6', 'true', '1']),
                ]:
                    sql = startup + f"CREATE SECRET fixture(TYPE s3, PROVIDER credential_chain, PROFILE '{profile}', {extra} REGION 'us-east-1', ENDPOINT '{endpoint}', USE_SSL false, URL_STYLE 'path');\n"
                    if case == 'refresh':
                        sql += "SET enable_logging=false; CALL enable_logging(storage='memory',level='info');\n"
                    sql += "SELECT sum(value) FROM read_csv('s3://fixture/input.csv', header=true, columns={'value':'INTEGER'});\n"
                    if case == 'profile-copy':
                        sql += f"COPY (SELECT 42 AS value) TO 's3://fixture/{engine}.parquet' (FORMAT parquet); SELECT value FROM read_parquet('s3://fixture/{engine}.parquet');"
                    else:
                        sql += "SELECT secret_string LIKE '%key_id=new-key%' FROM duckdb_secrets(redact=false); SELECT count(*) FROM duckdb_logs WHERE message LIKE 'Successfully refreshed secret: fixture, new key_id:%';"
                    run = subprocess.run([str(cli.resolve()), '-batch', '-bail', '-csv', '-noheader', '-unredacted', '-init', '/dev/null', ':memory:'],
                                         input=sql, env=fixture.environment, text=True, capture_output=True, timeout=90)
                    passed = run.returncode == 0 and run.stdout.strip().splitlines() == expected
                    if case == 'refresh':
                        passed = passed and counter.exists() and counter.read_text() == '2'
                    results.append(dict(engine=engine, case=case, sql=sql, exit=run.returncode, stdout=run.stdout,
                                        stderr=run.stderr, passed=passed))
                    (args.out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
            (args.out / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')
    passed = all(r['passed'] for r in results) and bool(requests) and all(r['valid_signature'] for r in requests)
    for engine in ['cpp', 'rust']:
        passed = passed and any(r['engine'] == engine and r['expired'] for r in requests)
        passed = passed and any(r['engine'] == engine and r['access_key'] == 'new-key' for r in requests)
    report = dict(passed=passed, cases=len(results), requests=len(requests),
                  invalid_signatures=sum(not r['valid_signature'] for r in requests),
                  scope='S3 credential-chain and process refresh against local emulator, with independent SigV4 validation')
    report['sha256'] = {}
    for name in ['cli', 'oracle', 'aws_extension', 'httpfs_extension']:
        with getattr(args, name).open('rb') as stream:
            report['sha256'][name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    if not passed:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
