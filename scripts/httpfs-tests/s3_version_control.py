"""Independent S3 region/version controls through the public C API.

Caller must authenticate both native engine and HTTPFS package before and after.
Synthetic credentials, loopback receiver/proxy only. --generate-fixtures is for
an independently authenticated C++ reference engine, never the port under test.
"""
import argparse
import base64
import ctypes
import hashlib
import hmac
import http.server
import json
import os
import re
import threading
import urllib.parse
import xml.etree.ElementTree as ET
from pathlib import Path


class API:
    def __init__(self, library, secrets):
        self.lib = ctypes.CDLL(str(library.resolve()))
        p = ctypes.c_void_p
        pp = ctypes.POINTER(p)

        class Result(ctypes.Structure):
            _fields_ = [('cols', ctypes.c_uint64), ('rows', ctypes.c_uint64),
                        ('changed', ctypes.c_uint64), ('columns', p), ('error', p), ('internal', p)]
        self.Result = Result
        self.lib.duckdb_library_version.restype = ctypes.c_char_p
        assert self.lib.duckdb_library_version() == b'v1.5.6'
        for name, arguments, result in [
            ('create_config', [pp], ctypes.c_int),
            ('set_config', [p, ctypes.c_char_p, ctypes.c_char_p], ctypes.c_int),
            ('open_ext', [ctypes.c_char_p, pp, p, pp], ctypes.c_int),
            ('connect', [p, pp], ctypes.c_int),
            ('query', [p, ctypes.c_char_p, ctypes.POINTER(Result)], ctypes.c_int),
            ('result_error', [ctypes.POINTER(Result)], ctypes.c_char_p),
            ('column_count', [ctypes.POINTER(Result)], ctypes.c_uint64),
            ('row_count', [ctypes.POINTER(Result)], ctypes.c_uint64),
            ('value_varchar', [ctypes.POINTER(Result), ctypes.c_uint64, ctypes.c_uint64], p),
            ('free', [p], None), ('destroy_result', [ctypes.POINTER(Result)], None),
            ('disconnect', [pp], None), ('close', [pp], None), ('destroy_config', [pp], None),
        ]:
            fn = getattr(self.lib, 'duckdb_' + name)
            fn.argtypes, fn.restype = arguments, result
        self.db, self.con, config, error = p(), p(), p(), p()
        assert self.lib.duckdb_create_config(ctypes.byref(config)) == 0
        for key, value in [('autoinstall_known_extensions', 'false'), ('autoload_known_extensions', 'false'),
                           ('allow_unsigned_extensions', 'false'), ('allow_community_extensions', 'false'),
                           ('secret_directory', str(secrets))]:
            assert self.lib.duckdb_set_config(config, key.encode(), value.encode()) == 0
        assert self.lib.duckdb_open_ext(None, ctypes.byref(self.db), config, ctypes.byref(error)) == 0
        self.lib.duckdb_destroy_config(ctypes.byref(config))
        assert self.lib.duckdb_connect(self.db, ctypes.byref(self.con)) == 0

    def query(self, sql):
        result = self.Result()
        status = self.lib.duckdb_query(self.con, sql.encode(), ctypes.byref(result))
        error = self.lib.duckdb_result_error(ctypes.byref(result))
        rows = []
        if status == 0:
            for r in range(self.lib.duckdb_row_count(ctypes.byref(result))):
                row = []
                for c in range(self.lib.duckdb_column_count(ctypes.byref(result))):
                    value = self.lib.duckdb_value_varchar(ctypes.byref(result), c, r)
                    row.append(ctypes.string_at(value).decode() if value else None)
                    self.lib.duckdb_free(value)
                rows.append(row)
        record = {'sql': sql, 'status': status, 'error': error.decode() if error else None, 'rows': rows}
        self.lib.duckdb_destroy_result(ctypes.byref(result))
        return record

    def close(self):
        self.lib.duckdb_disconnect(ctypes.byref(self.con))
        self.lib.duckdb_close(ctypes.byref(self.db))


def quote(value):
    return str(value).replace("'", "''")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['library', 'package', 'fixtures', 'out']:
        parser.add_argument(name, type=Path)
    parser.add_argument('--generate-fixtures', action='store_true')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    if args.generate_fixtures:
        args.fixtures.mkdir(parents=True, exist_ok=False)
        api = API(args.library, args.out / 'fixture-secrets')
        try:
            generated = []
            for name, expression in [('a', 'i'), ('b', 'i+1000')]:
                path = args.fixtures / (name + '.parquet')
                rec = api.query(f"COPY (SELECT ({expression})::BIGINT i,md5(i::VARCHAR) s FROM range(100000)t(i)) TO '{quote(path)}' (FORMAT PARQUET,COMPRESSION UNCOMPRESSED)")
                generated.append(rec)
                assert rec['status'] == 0, rec
            (args.fixtures / 'generation.json').write_text(json.dumps(generated, indent=2) + '\n')
        finally:
            api.close()
    payloads = [(args.fixtures / (name + '.parquet')).read_bytes() for name in ['a', 'b']]
    assert len(payloads[0]) == len(payloads[1]) and len(payloads[0]) > 4000000
    records, requests, active, failures = [], [], {}, []
    lock = threading.Lock()

    def authenticated(handler, body):
        match = re.fullmatch(r'AWS4-HMAC-SHA256 Credential=fixture-access/([^/]+)/([^/]+)/s3/aws4_request, SignedHeaders=([^,]+), Signature=([0-9a-f]{64})', handler.headers.get('Authorization', ''))
        if not match:
            return False, ''
        date, region, signed, signature = match.groups()
        names = signed.split(';')
        digest = hashlib.sha256(body).hexdigest()
        if names != sorted(names) or handler.headers.get('x-amz-content-sha256') != digest:
            return False, region
        parsed = urllib.parse.urlsplit(handler.path)
        canonical_query = parsed.query + '=' if parsed.query == 'uploads' else parsed.query
        headers = ''.join(name + ':' + ' '.join(handler.headers.get(name, '').split()) + '\n' for name in names)
        canonical = '\n'.join([handler.command, parsed.path, canonical_query, headers, signed, digest])
        scope = date + '/' + region + '/s3/aws4_request'
        message = '\n'.join(['AWS4-HMAC-SHA256', handler.headers.get('x-amz-date', ''), scope, hashlib.sha256(canonical.encode()).hexdigest()])
        key = b'AWS4fixture-secret'
        for value in [date, region, 's3', 'aws4_request']:
            key = hmac.new(key, value.encode(), hashlib.sha256).digest()
        return hmac.compare_digest(signature, hmac.new(key, message.encode(), hashlib.sha256).hexdigest()), region

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def handle(self):
            try:
                super().handle()
            except ConnectionResetError:
                # A client may close a pooled connection after consuming a response.
                pass

        def log_message(self, *unused):
            pass

        def do_CONNECT(self):
            if active.get('reject_connect'):
                self.send_response(405)
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            # ureq tunnels HTTP proxies; C++ sends absolute-form requests.
            # Both terminate here; this fixture never opens an upstream socket.
            self.send_response(200)
            self.send_header('Content-Length', '0')
            self.end_headers()

        def respond(self):
            body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
            valid, region = authenticated(self, body)
            parsed = urllib.parse.urlsplit(self.path)
            params = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
            listing = params.get('list-type') == ['2']
            with lock:
                status, response, headers = 200, b'', {}
                case = active['case']
                if not valid:
                    failures.append({'case': case, 'path': self.path, 'reason': 'signature'})
                    status = 403
                elif active.get('proxy_auth') and self.headers.get('Proxy-Authorization') != 'Basic ' + base64.b64encode(b'fixture-proxy-user:fixture-proxy-password').decode():
                    status = 407
                    headers['Proxy-Authenticate'] = 'Basic realm="fixture-proxy"'
                elif active.get('location') and not active.get('location_sent'):
                    status = 301
                    path = parsed.path + ('?' + parsed.query if parsed.query else '')
                    if active['location'] == 'root':
                        headers['Location'] = path
                    elif active['location'] == 'scheme':
                        headers['Location'] = '//' + self.headers['Host'] + path
                    elif active['location'] == 'relative':
                        headers['Location'] = path.rsplit('/', 1)[-1]
                    else:
                        headers['Location'] = 'http://' + self.headers['Host'] + path
                    active['location_sent'] = True
                elif active.get('redirect') and region != 'fixture-region':
                    status = active['redirect']
                    headers['x-amz-bucket-region'] = 'fixture-region'
                    # Omit Location: isolate S3 region handling from HTTP redirects.
                elif active.get('bad_list'):
                    status = 301
                    if active['bad_list'] != 'missing':
                        headers['x-amz-bucket-region'] = region if active['bad_list'] == 'same' else 'third-region'
                elif listing:
                    response = b'<ListBucketResult><Contents><Key>data.parquet</Key><Size>4400000</Size></Contents></ListBucketResult>'
                elif self.command == 'HEAD' and active.get('fallback'):
                    status = 405
                elif self.command == 'POST' and 'uploads' in params:
                    assert self.headers.get('x-amz-server-side-encryption') == 'aws:kms'
                    assert self.headers.get('x-amz-server-side-encryption-aws-kms-key-id') == 'fixture-kms'
                    response = b'<InitiateMultipartUploadResult><UploadId>fixture/upload+1=</UploadId></InitiateMultipartUploadResult>'
                elif self.command == 'PUT':
                    active['written'] = body
                    headers['ETag'] = '"written"'
                    if active.get('binary_response'):
                        response = payloads[0]
                    if active.get('op') == 'multipart':
                        assert params == {'partNumber': ['1'], 'uploadId': ['fixture/upload+1=']}, params
                elif self.command == 'POST' and 'uploadId' in params:
                    assert params == {'uploadId': ['fixture/upload+1=']}, params
                    root = ET.fromstring(body)
                    parts = [(item.findtext('{*}PartNumber'), item.findtext('{*}ETag')) for item in root.findall('{*}Part')]
                    assert parts == [('1', '"written"')], parts
                    response = b'<CompleteMultipartUploadResult><ETag>"complete"</ETag></CompleteMultipartUploadResult>'
                elif self.command == 'HEAD' or self.command == 'GET':
                    version = params.get('versionId', [''])[0]
                    current = active.get('current', 0)
                    selected = 0 if version == active.get('version') and version else current
                    response = payloads[selected]
                    tag = 'fixture-object-' + str(selected)
                    headers['ETag'] = tag if case == 'quote-equivalent' and self.command == 'GET' else '"' + tag + '"'
                    if active.get('version') and not (active.get('late') and self.command == 'HEAD'):
                        headers['x-amz-version-id'] = active['version'] if selected == 0 else 'replacement'
                    if self.command == 'GET' and self.headers.get('Range'):
                        start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                        response = response[start:end + 1]
                        headers['Content-Range'] = f'bytes {start}-{end}/{len(payloads[selected])}'
                        status = 206
                    if active.get('change') and (self.command == 'GET' if active.get('late') else self.command == 'HEAD'):
                        active['current'] = 1
                else:
                    status = 405
                requests.append({'case': case, 'method': self.command, 'path': self.path,
                                 'host': self.headers.get('Host'), 'region': region, 'valid': valid,
                                 'status': status, 'range': self.headers.get('Range'),
                                 'version': params.get('versionId'), 'body_sha256': hashlib.sha256(body).hexdigest()})
            self.send_response(status)
            self.send_header('Content-Length', str(len(response)))
            for name, value in headers.items():
                self.send_header(name, value)
            self.end_headers()
            if self.command != 'HEAD':
                self.wfile.write(response)

        do_HEAD = do_GET = do_PUT = do_POST = do_DELETE = respond

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    endpoint = f'127.0.0.1:{server.server_port}'
    keys = ['AWS_DEFAULT_REGION', 'AWS_REGION', 'AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY', 'AWS_SESSION_TOKEN',
            'DUCKDB_S3_ENDPOINT', 'DUCKDB_S3_USE_SSL', 'http_proxy', 'HTTP_PROXY', 'https_proxy', 'HTTPS_PROXY', 'NO_PROXY', 'no_proxy']
    saved = {k: os.environ.pop(k, None) for k in keys}
    cases = [
        ('pin-head', {'version': 'fixture/version+1=', 'change': True}),
        ('pin-fallback', {'version': 'fixture/version+1=', 'change': True, 'late': True, 'fallback': True}),
        ('pin-body-only', {'version': 'fixture/version+1=', 'change': True, 'late': True}),
        ('pin-null', {'version': 'null', 'change': True}),
        ('pin-absent', {}), ('pin-disabled', {'version': 'fixture/version+1=', 'change': True}),
        ('unsafe-disabled-pin', {'version': 'fixture/version+1=', 'change': True}),
        ('quote-equivalent', {}),
        ('location-same-origin', {'location': True}),
    ]
    cases += [(f'location-{kind}', {'location': kind}) for kind in ['root', 'scheme', 'relative']]
    cases += [(f'location-httplib-{kind}', {'location': kind, 'client': 'httplib'}) for kind in [True, 'root', 'scheme']]
    cases += [(f'{op}-{status}', {'redirect': status, 'op': op}) for op in ['read', 'list', 'put', 'multipart'] for status in [301, 400]]
    cases += [('put-default', {'op': 'put', 'tmp': None}), ('put-tmp-true', {'op': 'put', 'tmp': True})]
    cases += [('put-binary-response', {'op': 'put', 'binary_response': True})]
    cases += [(f'forward-proxy-{op}', {'proxy': 'path', 'reject_connect': True, 'op': op})
              for op in ['read', 'put', 'multipart']]
    cases += [('forward-proxy-auth', {'proxy': 'path', 'reject_connect': True, 'proxy_auth': True}),
              ('forward-proxy-large-put', {'proxy': 'path', 'reject_connect': True, 'op': 'put', 'large': True})]
    cases += [(f'aws-{style}', {'redirect': 301, 'proxy': style}) for style in ['path', 'vhost']]
    cases += [(f'list-{kind}', {'bad_list': kind, 'op': 'list', 'redirect': 301 if kind == 'repeat' else None}) for kind in ['missing', 'same', 'repeat']]
    try:
        for case, options in cases:
            active.clear()
            active.update(case=case, **options)
            api = API(args.library, args.out / case / 'secrets')
            first = len(requests)
            def query(label, sql, expected=None, success=True):
                record = {'case': case, 'label': label, **api.query(sql)}
                records.append(record)
                (args.out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                assert (record['status'] == 0) == success, record
                if expected is not None:
                    assert record['rows'] == expected, record
                return record
            try:
                query('load', f"LOAD '{quote(args.package / 'httpfs.duckdb_extension')}'")
                settings = [('s3_use_ssl', 'false'), ('s3_access_key_id', "'fixture-access'"),
                            ('s3_secret_access_key', "'fixture-secret'"), ('s3_region', "'wrong-region'" if options.get('redirect') else "'fixture-region'"),
                            ('http_retries', '0'), ('threads', '1'), ('s3_version_id_pinning', 'false' if case in ['pin-disabled', 'unsafe-disabled-pin'] else 'true')]
                if options.get('proxy'):
                    settings += [('http_proxy', "'http://" + endpoint + "'"), ('s3_url_style', "'" + options['proxy'] + "'")]
                    if options.get('proxy_auth'):
                        settings += [('http_proxy_username', "'fixture-proxy-user'"), ('http_proxy_password', "'fixture-proxy-password'")]
                else:
                    settings += [('s3_endpoint', "'" + endpoint + "'"), ('s3_url_style', "'path'")]
                if options.get('op') == 'multipart':
                    settings += [('s3_kms_key_id', "'fixture-kms'")]
                if options.get('client'):
                    settings += [('httpfs_client_implementation', "'" + options['client'] + "'")]
                if case == 'unsafe-disabled-pin':
                    settings += [('unsafe_disable_etag_checks', 'true')]
                for key, value in settings:
                    query(key, f'SET {key}={value}')
                if options.get('op') == 'list':
                    query('result', "SELECT file FROM glob('s3://bucket/*.parquet')", [['s3://bucket/data.parquet']] if not options.get('bad_list') else None, not bool(options.get('bad_list')))
                elif options.get('op') in ['put', 'multipart']:
                    tmp = ', USE_TMP_FILE true' if options.get('tmp') else '' if case == 'put-default' else ', USE_TMP_FILE false'
                    selection = "SELECT repeat('GET /payload HTTP/1.1',1000) s FROM range(20)" if options.get('large') else 'SELECT 7 i'
                    query('result', "COPY (" + selection + ") TO 's3://bucket/out.csv' (FORMAT CSV" + tmp + ")", [['20' if options.get('large') else '1']])
                    assert all('/bucket/out.csv' in x['path'] for x in requests[first:]), requests[first:]
                    expected_body = b's\n' + (b'GET /payload HTTP/1.1' * 1000 + b'\n') * 20 if options.get('large') else b'i\n7\n'
                    assert active['written'] == expected_body
                else:
                    etag_error = case in ['pin-disabled', 'pin-body-only']
                    result = query('result', "SELECT sum(i),sum(length(s)),count(*) FROM read_parquet('s3://bucket/data.parquet')", None if etag_error else [['5099950000' if case == 'unsafe-disabled-pin' else '4999950000', '3200000', '100000']], not etag_error)
                    if case == 'pin-body-only':
                        assert all(text in result['error'] for text in ['ETag on reading file', 'fixture-object-0', 'fixture-object-1']), result
                selected = requests[first:]
                assert selected and all(x['valid'] for x in selected), selected
                if options.get('location'):
                    assert [(x['method'], x['status']) for x in selected[:2]] == [('HEAD', 301), ('HEAD', 200)], selected
                if options.get('reject_connect'):
                    assert all(x['path'].startswith('http://') for x in selected), selected
                if case in ['pin-head', 'pin-null']:
                    assert all(x['version'] == [options['version']] for x in selected if x['method'] == 'GET'), selected
                if case == 'pin-fallback':
                    gets = [x for x in selected if x['method'] == 'GET']
                    assert len(gets) > 1 and gets[0]['version'] is None and all(x['version'] == [options['version']] for x in gets[1:]), gets
                if case in ['pin-absent', 'pin-disabled', 'pin-body-only', 'unsafe-disabled-pin']:
                    assert all(x['version'] is None for x in selected), selected
                if options.get('redirect') and not options.get('bad_list'):
                    assert selected[0]['region'] == 'wrong-region' and selected[0]['status'] == options['redirect'], selected
                    for index, request in enumerate(selected):
                        if request['region'] == 'wrong-region':
                            assert request['status'] == options['redirect'], selected
                            assert index + 1 < len(selected) and selected[index + 1]['region'] == 'fixture-region' and selected[index + 1]['method'] == request['method'], selected
                        else:
                            assert request['region'] == 'fixture-region', selected
                    if options.get('proxy'):
                        assert selected[0]['host'] != selected[1]['host'] and '.fixture-region.amazonaws.com' in selected[1]['host'], selected
            finally:
                api.close()
    finally:
        server.shutdown()
        server.server_close()
        for key, value in saved.items():
            if value is not None:
                os.environ[key] = value
        (args.out / 'records.json').write_text(json.dumps(records, indent=2) + '\n')
        (args.out / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')
        (args.out / 'fixtures.json').write_text(json.dumps([{'name': name, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()} for name, data in zip(['a.parquet', 'b.parquet'], payloads)], indent=2) + '\n')
    assert not failures, failures
    print(json.dumps({'cases': len(cases), 'records': len(records), 'requests': len(requests)}))


if __name__ == '__main__':
    main()
