"""S3 config-provider refresh controls with independent SigV4 validation.

Caller authenticates the engine/package before and after. Loopback receiver only;
all keys and refresh material are synthetic. Default listings must not expose
nested replacement secrets in the Rust engine.
"""
from pathlib import Path
import argparse, importlib.util, subprocess, sys, json, http.server, threading, re, urllib.parse, hashlib, hmac, os
parser = argparse.ArgumentParser(description=__doc__)
for name in ['library', 'package', 'parquet', 'out']:
    parser.add_argument(name, type=Path)
parser.add_argument('--engine', choices=['cpp', 'rust'], required=True)
args = parser.parse_args()
w = Path(__file__).resolve().parents[2]
p = args.out.resolve()
p.mkdir(parents=True, exist_ok=False)
package = args.package.resolve()
for key in ['http_proxy', 'HTTP_PROXY', 'https_proxy', 'HTTPS_PROXY', 'ALL_PROXY', 'all_proxy', 'NO_PROXY', 'no_proxy', 'AWS_DEFAULT_REGION', 'AWS_REGION', 'AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY', 'AWS_SESSION_TOKEN', 'DUCKDB_S3_ENDPOINT', 'DUCKDB_S3_USE_SSL']:
    os.environ.pop(key, None)
spec = importlib.util.spec_from_file_location('control', w / 'scripts/httpfs-tests/s3_version_control.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
payload = args.parquet.read_bytes()
active = {}
requests = []
records = []

def authenticated(handler, body):
    match = re.fullmatch('AWS4-HMAC-SHA256 Credential=(stale|fresh)/([^/]+)/([^/]+)/s3/aws4_request, SignedHeaders=([^,]+), Signature=([0-9a-f]{64})', handler.headers.get('Authorization', ''))
    if not match:
        return False
    identity, date, region, signed, signature = match.groups()
    names = signed.split(';')
    digest = hashlib.sha256(body).hexdigest()
    if names != sorted(names) or handler.headers.get('x-amz-content-sha256') != digest:
        return False
    parsed = urllib.parse.urlsplit(handler.path)
    query = parsed.query + '=' if parsed.query == 'uploads' else parsed.query
    headers = ''.join((name + ':' + ' '.join(handler.headers.get(name, '').split()) + '\n' for name in names))
    canonical = '\n'.join([handler.command, parsed.path, query, headers, signed, digest])
    scope = date + '/' + region + '/s3/aws4_request'
    message = '\n'.join(['AWS4-HMAC-SHA256', handler.headers.get('x-amz-date', ''), scope, hashlib.sha256(canonical.encode()).hexdigest()])
    key = ('AWS4' + identity + '-secret').encode()
    for value in [date, region, 's3', 'aws4_request']:
        key = hmac.new(key, value.encode(), hashlib.sha256).digest()
    return hmac.compare_digest(signature, hmac.new(key, message.encode(), hashlib.sha256).hexdigest())

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
        authorization = self.headers.get('Authorization', '')
        match = re.search('Credential=([^/]+)/', authorization)
        key = match[1] if match else ''
        status = 200
        data = b''
        headers = {}
        parsed = urllib.parse.urlsplit(self.path)
        params = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
        valid = authenticated(self, body)
        if not valid:
            status = 403
        elif (active['case'] == 'endpoint' and self.server is server) or (key == 'stale' and (active['failure_method'] == 'ALL' or self.command == active['failure_method'])):
            status = active['status']
            data = ('<Error><Code>' + active['code'] + '</Code></Error>').encode()
        elif params.get('list-type') == ['2']:
            data = b'<ListBucketResult><Contents><Key>data.parquet</Key><Size>4400000</Size></Contents></ListBucketResult>'
        elif self.command == 'HEAD' or self.command == 'GET':
            data = payload
            headers['ETag'] = '"fixture"'
            if self.command == 'GET' and self.headers.get('Range'):
                first, last = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                data = data[first:last + 1]
                status = 206
                headers['Content-Range'] = f'bytes {first}-{last}/{len(payload)}'
        elif self.command == 'POST' and 'uploads' in params:
            data = b'<InitiateMultipartUploadResult><UploadId>fixture</UploadId></InitiateMultipartUploadResult>'
        elif self.command == 'POST' and 'uploadId' in params:
            data = b'<CompleteMultipartUploadResult><ETag>"written"</ETag></CompleteMultipartUploadResult>'
        elif self.command == 'PUT':
            headers['ETag'] = '"written"'
            active['written'] = body
        requests.append({'case': active['case'], 'method': self.command, 'key': key, 'status': status, 'path': self.path, 'signature_valid': valid, 'port': self.server.server_port})
        self.send_response(status)
        self.send_header('Content-Length', str(len(data)))
        for k, v in headers.items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != 'HEAD':
            self.wfile.write(data)
    do_GET = do_HEAD = do_PUT = do_POST = respond
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
alternate = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
for receiver in [server, alternate]:
    threading.Thread(target=receiver.serve_forever, daemon=True).start()
try:
    cases = [('403', 403, 'AccessDenied', True, True), ('401', 401, 'AccessDenied', True, True), ('expired', 400, 'ExpiredToken', True, True), ('other400', 400, 'BadRequest', True, True), ('disabled', 403, 'AccessDenied', False, True), ('no-info', 403, 'AccessDenied', True, False)]
    cases += [(kind,403,'AccessDenied',True,True) for kind in ['get','put','multipart','list','endpoint','unchanged','auto','bad-bool']]
    for kind, status, code, enabled, info in cases:
        active.clear()
        active.update(case=kind, status=status, code=code, failure_method='GET' if kind in ['get','list'] else 'PUT' if kind == 'put' else 'POST' if kind == 'multipart' else 'ALL')
        api = m.API(args.library, p / kind / 'secrets')
        events = len(requests)
        setup = [f"SET extension_directory='{m.quote(p / kind / 'extensions')}'", f"LOAD '{m.quote(package / 'httpfs.duckdb_extension')}'", 'SET s3_use_ssl=false', "SET s3_region='fixture-region'", f"SET s3_endpoint='127.0.0.1:{server.server_port}'", "SET s3_url_style='path'", 'SET http_retries=0', 'SET threads=1', f"SET httpfs_enable_credential_refresh={('true' if enabled else 'false')}"]
        for sql in setup:
            r = api.query(sql)
            records.append({'case': kind, 'label': 'setup', **r})
            assert r['status'] == 0, r
        if kind == 'multipart':
            assert api.query("SET s3_kms_key_id='fixture-kms'")['status'] == 0
        refresh = ", REFRESH_INFO MAP {'key_id':'fresh','secret':'fresh-secret','region':'fixture-region'}" if info else ''
        if kind == 'endpoint':
            refresh = ", REFRESH_INFO MAP {'key_id':'fresh','secret':'fresh-secret','region':'fixture-region','endpoint':'127.0.0.1:" + str(alternate.server_port) + "'}"
        elif kind == 'unchanged':
            refresh = ", REFRESH_INFO MAP {'key_id':'stale','secret':'stale-secret','region':'fixture-region'}"
        elif kind == 'auto':
            refresh = ", REFRESH 'auto'"
        elif kind == 'bad-bool':
            refresh = ", REFRESH_INFO MAP {'key_id':'fresh','secret':'fresh-secret','use_ssl':'false'}"
        r = api.query("CREATE SECRET credentials(TYPE s3,KEY_ID 'stale',SECRET 'stale-secret',SCOPE 's3://bucket/'" + refresh + ')')
        records.append({'case': kind, 'label': 'create', **r})
        assert r['status'] == 0, r
        operation = "COPY (SELECT 7 i) TO 's3://bucket/out.csv' (FORMAT CSV,USE_TMP_FILE false)" if kind in ['put','multipart'] else "SELECT file FROM glob('s3://bucket/*.parquet')" if kind == 'list' else "SELECT sum(i) FROM read_parquet('s3://bucket/data.parquet')"
        for label, sql in [('before', 'SELECT secret_string FROM duckdb_secrets()'), ('read', operation), ('after', 'SELECT secret_string FROM duckdb_secrets()')]:
            r = api.query(sql)
            records.append({'case': kind, 'label': label, **r})
            if label == 'read':
                success = enabled and info and kind not in ['unchanged','auto','bad-bool'] and (status in [401, 403] or code == 'ExpiredToken')
                assert (r['status'] == 0) == success, r
                if kind == 'bad-bool':
                    assert "Invalid type past to secret option: 'use_ssl', found 'VARCHAR', expected: 'BOOLEAN'" in r['error'], r
                if success:
                    assert r['rows'] == ([['1']] if kind in ['put','multipart'] else [['s3://bucket/data.parquet']] if kind == 'list' else [['4999950000']]), r
            if label == 'before' and args.engine == 'rust':
                assert 'fresh-secret' not in str(r['rows']), r
            if label == 'after':
                assert ('key_id=fresh' in str(r['rows'])) == (enabled and info and kind not in ['unchanged','auto','bad-bool'] and (status in [401, 403] or code == 'ExpiredToken')), r
        assert all((x['signature_valid'] for x in requests[events:])), requests[events:]
        if kind in ['put','multipart']:
            assert active['written'] == b'i\n7\n'
        api.close()
        (p / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        print('REQUESTS', requests[events:], flush=True)
finally:
    for receiver in [server,alternate]:
        receiver.shutdown()
        receiver.server_close()
    (p / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')
(p / 'terminal.json').write_text(json.dumps({'exit_code': 0, 'cases': len(cases), 'records': len(records), 'scope': 'Config-provider refresh with independentSigV4 validation and private storage;not fullprovider/lifecycle/platform/release acceptance.'}) + '\n')
