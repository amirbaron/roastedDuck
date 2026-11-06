"""Synthetic S3 redirect policy regression through public C API.

Caller must authenticate the native engine and HTTPFS package before and after.
Two loopback receivers never open upstream sockets. All credentials are synthetic.
Destination deliberately accepts forwarded signatures without revalidation so method
and credential transformations can be observed; use s3_version_control.py for
independent SigV4 verification. Cross-origin tokens are forbidden for the Rust run.
"""
from pathlib import Path
import argparse, importlib.util, json, subprocess, sys, os, http.server, threading, urllib.parse, hashlib
parser = argparse.ArgumentParser(description=__doc__)
for name in ['library', 'package', 'parquet', 'out']:
    parser.add_argument(name, type=Path)
parser.add_argument('--engine', choices=['cpp', 'rust'], required=True)
args = parser.parse_args()
w = Path(__file__).resolve().parents[2]
p = args.out.resolve()
p.mkdir(parents=True, exist_ok=False)
package = args.package.resolve()
spec = importlib.util.spec_from_file_location('api', w / 'scripts/httpfs-tests/s3_version_control.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
data = args.parquet.read_bytes()
active = {}
events = []
records = []

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *unused):
        pass

    def handle(self):
        try:
            super().handle()
        except (ConnectionResetError, BrokenPipeError):
            pass

    def respond(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        parsed = urllib.parse.urlsplit(self.path)
        params = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
        target = parsed.path.startswith('/redirected')
        status = 200
        response = b''
        headers = {}
        if self.command == active['method'] and (not target) and (not active.get('sent')):
            status = active['status']
            port = second.server_port if active['cross'] else first.server_port
            headers['Location'] = f'http://127.0.0.1:{port}/redirected' + parsed.path + ('?' + parsed.query if parsed.query else '')
            active['sent'] = True
        elif self.command == 'POST' and 'uploads' in params:
            response = b'<InitiateMultipartUploadResult><UploadId>fixture</UploadId></InitiateMultipartUploadResult>'
        elif self.command == 'POST' and 'uploadId' in params:
            response = b'<CompleteMultipartUploadResult><ETag>"done"</ETag></CompleteMultipartUploadResult>'
        elif self.command == 'PUT':
            headers['ETag'] = '"written"'
        elif self.command in ['GET', 'HEAD']:
            response = data
            headers['ETag'] = '"fixture"'
            if self.headers.get('Range'):
                start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                response = response[start:end + 1]
                headers['Content-Range'] = f'bytes {start}-{end}/{len(data)}'
                status = 206
        events.append({'case': active['case'], 'method': self.command, 'path': self.path, 'status': status, 'target': target, 'host': self.headers.get('Host'), 'authorization': self.headers.get('Authorization'), 'session_token': self.headers.get('x-amz-security-token'), 'range': self.headers.get('Range'), 'body_bytes': len(body), 'body_sha256': hashlib.sha256(body).hexdigest()})
        self.send_response(status)
        self.send_header('Content-Length', str(len(response)))
        for name, value in headers.items():
            self.send_header(name, value)
        self.end_headers()
        if self.command != 'HEAD':
            self.wfile.write(response)
    do_HEAD = do_GET = do_POST = do_PUT = respond
first = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
second = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
for server in [first, second]:
    threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    for implementation in ['default', 'httplib']:
        for cross in [False, True]:
            for status in [301, 302, 303, 307, 308]:
                for method in ['HEAD', 'GET', 'PUT', 'POST']:
                    case = f'{implementation}-cross{int(cross)}-{status}-{method}'
                    active.clear()
                    active.update(case=case, method=method, cross=cross, status=status)
                    api = m.API(args.library, p / case / 'secrets')
                    first_event = len(events)
                    setup = []
                    for sql in [f"SET extension_directory='{m.quote(p / case / 'extensions')}'", f"LOAD '{m.quote(package / 'httpfs.duckdb_extension')}'", f"SET httpfs_client_implementation='{implementation}'", 'SET s3_use_ssl=false', "SET s3_access_key_id='fixture-access'", "SET s3_secret_access_key='fixture-secret'", "SET s3_session_token='fixture-session'", "SET s3_region='fixture-region'", f"SET s3_endpoint='127.0.0.1:{first.server_port}'", "SET s3_url_style='path'", 'SET http_retries=0', 'SET http_timeout=5', 'SET threads=1']:
                        r = api.query(sql)
                        setup.append(r)
                        assert r['status'] == 0, r
                    if method == 'POST':
                        assert api.query("SET s3_kms_key_id='fixture-kms'")['status'] == 0
                    sql = "COPY (SELECT 7 i) TO 's3://bucket/out.csv' (FORMAT CSV,USE_TMP_FILE false)" if method in ['PUT', 'POST'] else "SELECT sum(i) FROM read_parquet('s3://bucket/data.parquet')"
                    result = api.query(sql)
                    api.close()
                    selected = events[first_event:]
                    targets = [e for e in selected if e['target']]
                    record = {'case': case, 'implementation': implementation, 'status': status, 'original_method': method, 'cross': cross, 'redirect_sent': active.get('sent', False), 'result': result, 'events': selected, 'target_methods': [e['method'] for e in targets], 'target_authorization': [e['authorization'] is not None for e in targets], 'target_session_tokens': [e['session_token'] for e in targets]}
                    expected_method = 'GET' if implementation == 'httplib' and status == 303 and (method not in ['GET', 'HEAD']) or (implementation == 'default' and method == 'POST' and (status in [301, 302, 303])) else method
                    assert record['redirect_sent'] and record['target_methods'] == [expected_method], record
                    cleared = implementation == 'httplib' and status == 303 and (method not in ['GET', 'HEAD'])
                    assert record['target_authorization'] == [not cross and (not cleared)], record
                    if args.engine == 'rust' and cross:
                        assert record['target_session_tokens'] == [None], record
                    elif not cross:
                        assert record['target_session_tokens'] == [None if cleared else 'fixture-session'], record
                    failed_init = method == 'POST' and expected_method == 'GET'
                    assert result['status'] == int(failed_init), record
                    if not failed_init:
                        assert result['rows'] == ([['4999950000']] if method in ['HEAD', 'GET'] else [['1']]), record
                    records.append(record)
                    (p / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                    print(case, record['target_methods'], record['target_authorization'], record['target_session_tokens'], result['status'], flush=True)
finally:
    for server in [first, second]:
        server.shutdown()
        server.server_close()
    (p / 'events.json').write_text(json.dumps(events, indent=2) + '\n')
assert len(records) == 80
(p / 'terminal.json').write_text(json.dumps({'exit_code': 0, 'cases': 80, 'engine': args.engine, 'scope': '80 checked public-CAPI redirect method/outcome/auth cases with synthetic credentials. No upstream sockets. Rust strips cross-origin tokens; C++ token behavior observed, not required. Not final release/platform or SigV4-validation proof.'}) + '\n')
