"""Public C API S3 read/secret controls against an independent SigV4 verifier.

The caller MUST authenticate the native library and extension package before and
after execution. All credentials are synthetic and only a loopback server is used.
Optional --listing adds paginated/hierarchical wildcard controls. This is not
multipart-write or AWS service compatibility evidence. Parquet fixtures must
come from the independent C++ engine.
"""
import argparse
import ctypes
import hashlib
import hmac
import http.server
import json
import os
import re
import threading
import urllib.parse
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    parser.add_argument('package', type=Path)
    parser.add_argument('parquet', type=Path)
    parser.add_argument('out', type=Path)
    parser.add_argument('--listing', action='store_true')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    library = args.library.resolve()
    lib = ctypes.CDLL(str(library))
    vp = ctypes.c_void_p
    pp = ctypes.POINTER(vp)

    class Result(ctypes.Structure):
        _fields_ = [('cols', ctypes.c_uint64), ('rows', ctypes.c_uint64),
                    ('changed', ctypes.c_uint64), ('columns', vp), ('error', vp), ('internal', vp)]

    lib.duckdb_library_version.restype = ctypes.c_char_p
    assert lib.duckdb_library_version() == b'v1.5.6'
    for name, params, result in [
        ('duckdb_create_config', [pp], ctypes.c_int),
        ('duckdb_set_config', [vp, ctypes.c_char_p, ctypes.c_char_p], ctypes.c_int),
        ('duckdb_open_ext', [ctypes.c_char_p, pp, vp, pp], ctypes.c_int),
        ('duckdb_connect', [vp, pp], ctypes.c_int),
        ('duckdb_query', [vp, ctypes.c_char_p, ctypes.POINTER(Result)], ctypes.c_int),
        ('duckdb_result_error', [ctypes.POINTER(Result)], ctypes.c_char_p),
        ('duckdb_column_count', [ctypes.POINTER(Result)], ctypes.c_uint64),
        ('duckdb_row_count', [ctypes.POINTER(Result)], ctypes.c_uint64),
        ('duckdb_value_is_null', [ctypes.POINTER(Result), ctypes.c_uint64, ctypes.c_uint64], ctypes.c_bool),
        ('duckdb_value_varchar', [ctypes.POINTER(Result), ctypes.c_uint64, ctypes.c_uint64], vp),
    ]:
        fn = getattr(lib, name)
        fn.argtypes = params
        fn.restype = result
    lib.duckdb_destroy_result.argtypes = [ctypes.POINTER(Result)]
    lib.duckdb_free.argtypes = [vp]
    for name in ['duckdb_disconnect', 'duckdb_close', 'duckdb_destroy_config']:
        getattr(lib, name).argtypes = [pp]

    records = []
    requests = []
    data = {'data.csv': b'i,s\n1,one\n2,two\n', 'data.parquet': args.parquet.read_bytes(),
            'a b+%#é.csv': b'i,s\n3,three\n4,four\n', 'a*?.csv': b'i,s\n1,one\n2,two\n'}
    objects = {key: data['data.csv'] for key in [
        'flat/a.csv', 'flat/b.csv', 'flat/aa.csv', 'flat/ab.csv', 'flat/a.txt', 'flat/a*.csv',
        'nested/a/file1.csv', 'nested/a/deep/file2.csv', 'nested/b/file3.csv',
        'encoded/a b+%é&.csv', 'encoded/bracket[1].csv',
    ]}
    objects.update({f'paged/{n:02}.csv': data['data.csv'] for n in range(23)})
    objects.update({f'partial/{n:02}.csv': data['data.csv'] for n in range(8)})
    objects.update({f'dense/{year}/{n:03}.csv': data['data.csv'] for year in [2020, 2021] for n in range(260)})
    objects.update({'parquet/a.parquet': data['data.parquet'], 'parquet/deep/b.parquet': data['data.parquet']})
    objects['nested/empty/'] = b''  # S3 directory-marker objects are not files.
    credential_id = 'fixture-access'
    credential_secret = b'fixture-secret'
    token = 'fixture-session'
    active = {}

    def validate(handler):
        auth = handler.headers.get('Authorization', '')
        if active.get('public'):
            return not auth, 'public'
        if active.get('bearer'):
            return auth == 'Bearer fixture-bearer', 'bearer'
        match = re.fullmatch(r'AWS4-HMAC-SHA256 Credential=([^/]+)/([^/]+)/([^/]+)/s3/aws4_request, SignedHeaders=([^,]+), Signature=([0-9a-f]{64})', auth)
        if not match:
            return False, 'missing-signature'
        key, date, region, signed, signature = match.groups()
        names = signed.split(';')
        if key != credential_id or names != sorted(names) or region != 'fixture-region':
            return False, 'credential-or-headers'
        amzdate = handler.headers.get('x-amz-date', '')
        if not re.fullmatch(date + r'T\d{6}Z', amzdate):
            return False, 'date'
        if active.get('session') and handler.headers.get('x-amz-security-token') != token:
            return False, 'session'
        if active.get('requester') and handler.headers.get('x-amz-request-payer') != 'requester':
            return False, 'requester'
        if active.get('extra') and handler.headers.get('X-Fixture-Key') != 'fixture-header':
            return False, 'extra'
        payload = hashlib.sha256(b'').hexdigest()
        if handler.headers.get('x-amz-content-sha256') != payload:
            return False, 'payload'
        canonical_headers = ''.join(name + ':' + ' '.join(handler.headers.get(name, '').split()) + '\n' for name in names)
        path = urllib.parse.urlsplit(handler.path)
        canonical = '\n'.join([handler.command, path.path, path.query, canonical_headers, signed, payload])
        scope = '/'.join([date, region, 's3', 'aws4_request'])
        to_sign = '\n'.join(['AWS4-HMAC-SHA256', amzdate, scope, hashlib.sha256(canonical.encode()).hexdigest()])
        signing_key = b'AWS4' + credential_secret
        for value in [date, region, 's3', 'aws4_request']:
            signing_key = hmac.new(signing_key, value.encode(), hashlib.sha256).digest()
        expected = hmac.new(signing_key, to_sign.encode(), hashlib.sha256).hexdigest()
        return hmac.compare_digest(signature, expected), 'signature'

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *unused):
            pass

        def do_HEAD(self):
            self.respond(False)

        def do_GET(self):
            self.respond(True)

        def respond(self, body):
            valid, stage = validate(self)
            parsed = urllib.parse.urlsplit(self.path)
            path = urllib.parse.unquote(parsed.path)
            params = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
            listing = params.get('list-type') == ['2']
            name = path.rsplit('/', 1)[-1]
            object_key = path.removeprefix('/bucket/')
            payload = objects.get(object_key, data.get(name))
            forced_status = None
            if valid and listing:
                assert path == '/bucket/' and self.command == 'GET', (path, self.command)
                assert params.get('encoding-type') == ['url'], params
                prefix = params['prefix'][0]
                delimiter = params.get('delimiter', [''])[0]
                assert delimiter in ['', '/'], delimiter
                entries = set()
                for key in objects:
                    if key.startswith(prefix):
                        suffix = key[len(prefix):]
                        if delimiter and '/' in suffix:
                            entries.add(('prefix', prefix + suffix.split('/', 1)[0] + '/'))
                        else:
                            entries.add(('key', key))
                entries = sorted(entries, key=lambda entry: entry[1])
                continuation = params.get('continuation-token', [''])[0]
                start_index = 0
                if continuation:
                    match = re.fullmatch(r'fixture/(\d+)\+next=', continuation)
                    assert match, continuation
                    start_index = int(match[1])
                limit = 3 if prefix.startswith(('paged/', 'partial/')) else 128
                selected = entries[start_index:start_index + limit]
                encode_key = lambda value: urllib.parse.quote(value, safe='')
                xml = ['<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">']
                for kind, key in selected:
                    if kind == 'prefix':
                        xml.append('<CommonPrefixes><Prefix>' + encode_key(key) + '</Prefix></CommonPrefixes>')
                    else:
                        xml.append('<Contents><Key>' + encode_key(key) + '</Key><LastModified>2020-01-01T00:00:00.000Z</LastModified><ETag>&quot;fixture-s3-object-v1&quot;</ETag><Size>' + str(len(objects[key])) + '</Size></Contents>')
                if start_index + limit < len(entries):
                    xml.append('<NextContinuationToken>fixture/' + str(start_index + limit) + '+next=</NextContinuationToken>')
                xml.append('</ListBucketResult>')
                payload = ''.join(xml).encode()
                if prefix.startswith(('denied/', 'invalid-region/', 'missing-bucket/', 'truncated-error/')) or prefix.startswith('partial/') and start_index:
                    forced_status = 400 if prefix.startswith('invalid-region/') else 404 if prefix.startswith('missing-bucket/') else 403
                    code = 'InvalidAccessKeyId' if prefix.startswith('denied/') else 'NoSuchBucket' if forced_status == 404 else 'AccessDenied'
                    payload = ('<Error><Code>' + code + '</Code><Message>fixture rejected</Message><AWSAccessKeyId>fixture-access</AWSAccessKeyId></Error>').encode()
                    if prefix.startswith('truncated-error/'):
                        payload = b'<Error><Code>AccessDenied</Code><Message>incomplete'
            status = forced_status or 200
            if not valid:
                status, payload = 403, b'<Error><Code>SignatureDoesNotMatch</Code><Message>fixture rejection</Message></Error>'
            elif payload is None:
                status, payload = 404, b'<Error><Code>NoSuchKey</Code></Error>'
            start, end = 0, len(payload) - 1
            if body and status == 200 and self.headers.get('Range'):
                match = re.fullmatch(r'bytes=(\d+)-(\d+)', self.headers['Range'])
                assert match, self.headers['Range']
                start, end = int(match[1]), min(int(match[2]), len(payload) - 1)
                status = 206
            requests.append({'case': active['case'], 'method': self.command,
                             'path': self.path, 'range': self.headers.get('Range'),
                             'extra_header': self.headers.get('X-Fixture-Key'),
                             'listing': listing,
                             'authentication_valid': valid, 'validation_stage': stage, 'status': status})
            self.send_response(status)
            self.send_header('Content-Length', str(end - start + 1))
            self.send_header('ETag', '"fixture-s3-object-v1"')
            self.send_header('Last-Modified', 'Wed, 01 Jan 2020 00:00:00 GMT')
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(payload)}')
            self.end_headers()
            if body:
                self.wfile.write(payload[start:end + 1])

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    endpoint = '127.0.0.1:' + str(server.server_port)
    quote = lambda value: str(value).replace("'", "''")
    native_identity = {'path': str(library), 'sha256': hashlib.file_digest(library.open('rb'), 'sha256').hexdigest()}
    cases = []

    def query(label, sql, expected=None, success=True):
        result = Result()
        status = lib.duckdb_query(con, sql.encode(), ctypes.byref(result))
        error = lib.duckdb_result_error(ctypes.byref(result))
        rows = []
        if status == 0:
            for r in range(lib.duckdb_row_count(ctypes.byref(result))):
                row = []
                for c in range(lib.duckdb_column_count(ctypes.byref(result))):
                    if lib.duckdb_value_is_null(ctypes.byref(result), c, r):
                        row.append(None)
                    else:
                        ptr = lib.duckdb_value_varchar(ctypes.byref(result), c, r)
                        row.append(ctypes.string_at(ptr).decode())
                        lib.duckdb_free(ptr)
                rows.append(row)
        record = {'case': active['case'], 'label': label, 'sql': sql, 'status': status,
                  'error': error.decode() if error else None, 'rows': rows}
        records.append(record)
        lib.duckdb_destroy_result(ctypes.byref(result))
        (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        assert (status == 0) == success, record
        if expected is not None:
            assert rows == expected, record
        return record

    environment_keys = ['AWS_DEFAULT_REGION', 'AWS_REGION', 'AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY',
                        'AWS_SESSION_TOKEN', 'DUCKDB_S3_ENDPOINT', 'DUCKDB_S3_USE_SSL',
                        'DUCKDB_S3_KMS_KEY_ID', 'DUCKDB_S3_REQUESTER_PAYS']
    saved_environment = {key: os.environ.get(key) for key in environment_keys}
    case_names = ['settings', 'secret', 'session', 'requester', 'scope', 'aws', 'r2',
                     'gcs', 'gcs-bearer', 'public', 's3a', 's3n', 'encoded-key', 'query', 'secret-extra',
                     'environment', 'disable-global', 'http-fallback', 'http-disabled', 'http-bearer', 'compatibility']
    if args.listing:
        case_names += ['glob-flat', 'glob-recursive', 'glob-paged', 'glob-dense', 'glob-flat-strategy', 'glob-query', 'glob-read', 'glob-errors', 'glob-limit']
    try:
        for kind in case_names:
            for key in environment_keys:
                os.environ.pop(key, None)
            if kind in ['environment', 'disable-global']:
                os.environ.update(AWS_DEFAULT_REGION='wrong-default-region', AWS_REGION='fixture-region',
                                  AWS_ACCESS_KEY_ID=credential_id, AWS_SECRET_ACCESS_KEY=credential_secret.decode(),
                                  AWS_SESSION_TOKEN=token, DUCKDB_S3_ENDPOINT=endpoint, DUCKDB_S3_USE_SSL='false')
            active.clear()
            active.update(case=kind, public=kind in ['public', 'disable-global'], bearer=kind == 'gcs-bearer',
                          session=kind in ['session', 'environment'], requester=kind == 'requester',
                          extra=kind in ['secret-extra', 'http-fallback'])
            db, con, config, error = vp(), vp(), vp(), vp()
            start = len(records)
            assert lib.duckdb_create_config(ctypes.byref(config)) == 0
            for key, value in [('allow_unsigned_extensions', 'false'), ('allow_community_extensions', 'false'),
                               ('autoinstall_known_extensions', 'false'), ('autoload_known_extensions', 'false'),
                               ('secret_directory', str(out / kind / 'secrets'))]:
                assert lib.duckdb_set_config(config, key.encode(), value.encode()) == 0
            assert lib.duckdb_open_ext(None, ctypes.byref(db), config, ctypes.byref(error)) == 0
            lib.duckdb_destroy_config(ctypes.byref(config))
            assert lib.duckdb_connect(db, ctypes.byref(con)) == 0
            try:
                query('load', f"LOAD '{quote(args.package / 'httpfs.duckdb_extension')}'")
                scheme = kind if kind in ['r2', 'gcs', 's3a', 's3n'] else 's3'
                if kind == 'gcs-bearer':
                    scheme = 'gs'
                secret_kind = 'gcs' if kind == 'gcs-bearer' else kind if kind in ['aws', 'r2', 'gcs'] else 's3'
                common = [('s3_endpoint', endpoint), ('s3_url_style', 'path'), ('s3_region', 'fixture-region')]
                if kind.startswith('glob-') or kind in ['settings', 's3a', 's3n', 'encoded-key', 'query', 'http-fallback', 'http-disabled', 'http-bearer', 'compatibility']:
                    for key, value in common + [('s3_access_key_id', credential_id), ('s3_secret_access_key', credential_secret.decode())]:
                        query(key, f"SET {key}='{quote(value)}'")
                    query('ssl', 'SET s3_use_ssl=false')
                    if kind.startswith('http-'):
                        query('http-secret', "CREATE SECRET http_credentials (TYPE http, SCOPE 's3://', BEARER_TOKEN 'must-not-replace-sigv4', EXTRA_HTTP_HEADERS MAP {'X-Fixture-Key':'fixture-header'})")
                        if kind == 'http-disabled':
                            query('disable-http', 'SET merge_http_secret_into_s3_request=false')
                    if kind == 'compatibility':
                        query('compatibility', 'SET s3_url_compatibility_mode=true')
                elif kind in ['public', 'disable-global']:
                    for key, value in common:
                        query(key, f"SET {key}='{quote(value)}'")
                    query('ssl', 'SET s3_use_ssl=false')
                    if kind == 'disable-global':
                        query('disable-global', 'SET enable_global_s3_configuration=false')
                elif kind == 'environment':
                    query('style', "SET s3_url_style='path'")
                    query('environment-region', "SELECT current_setting('s3_region')", [['fixture-region']])
                else:
                    options = f"TYPE {secret_kind}, KEY_ID '{credential_id}', SECRET '{credential_secret.decode()}', REGION 'fixture-region', ENDPOINT '{endpoint}', URL_STYLE 'path', USE_SSL false"
                    if kind == 'scope':
                        query('global-wrong', "SET s3_secret_access_key='wrong-global'")
                        options += ", SCOPE 's3://bucket/'"
                    if kind == 'session':
                        options += f", SESSION_TOKEN '{token}'"
                    if kind == 'requester':
                        options += ', REQUESTER_PAYS true'
                    if kind == 'gcs-bearer':
                        options += ", BEARER_TOKEN 'fixture-bearer'"
                    if kind == 'secret-extra':
                        options += ", EXTRA_HTTP_HEADERS MAP {'X-Fixture-Key':'fixture-header'}"
                    query('create-secret', f'CREATE SECRET credentials ({options})')
                    query('secret-redaction', "SELECT type, provider, secret_string FROM duckdb_secrets() ORDER BY name")
                suffix = ''
                if kind == 'query':
                    suffix = '?s3_region=fixture-region&s3_endpoint=' + urllib.parse.quote(endpoint, safe='')
                if kind.startswith('glob-'):
                    def glob(label, pattern, expected):
                        query(label, f"SELECT file FROM glob('s3://bucket/{pattern}') ORDER BY file", [[name] for name in expected])
                    if kind == 'glob-flat':
                        glob('flat', 'flat/*.csv', ['s3://bucket/' + key for key in sorted(objects) if key.startswith('flat/') and key.endswith('.csv')])
                        glob('bracket', 'flat/[ab].csv', ['s3://bucket/flat/a.csv', 's3://bucket/flat/b.csv'])
                        glob('invert', 'flat/[!a]*.csv', ['s3://bucket/flat/b.csv'])
                        glob('escaped', r'flat/a\*.csv', ['s3://bucket/flat/a*.csv'])
                        glob('empty', 'no-such-prefix/*.csv', [])
                        glob('encoded', 'encoded/*.csv', ['s3://bucket/' + key for key in sorted(objects) if key.startswith('encoded/')])
                        local = out / 'local'
                        local.mkdir()
                        for name in ['a.csv', 'b.csv', 'c.txt']:
                            (local / name).write_bytes(data['data.csv'])
                        query('local-shared-matcher', f"SELECT count(*) FROM glob('{quote(local)}/[a-b].csv')", [['2']])
                    elif kind == 'glob-recursive':
                        glob('recursive', 'nested/**/*.csv', ['s3://bucket/' + key for key in sorted(objects) if key.startswith('nested/') and key.endswith('.csv')])
                        glob('one-level', 'nested/*/*.csv', ['s3://bucket/nested/a/file1.csv', 's3://bucket/nested/b/file3.csv'])
                        glob('directory-markers', 'nested/**', ['s3://bucket/' + key for key in sorted(objects) if key.startswith('nested/') and not key.endswith('/')])
                    elif kind in ['glob-dense', 'glob-flat-strategy', 'glob-paged']:
                        prefix = 'paged' if kind == 'glob-paged' else 'dense'
                        if kind == 'glob-flat-strategy':
                            query('flat-strategy', 'SET s3_allow_recursive_globbing=false')
                        query('count', f"SELECT count(*) FROM glob('s3://bucket/{prefix}/{'*' if prefix == 'paged' else '*/*'}.csv')", [['23' if prefix == 'paged' else '520']])
                    elif kind == 'glob-query':
                        suffix = '?s3_region=fixture-region&s3_endpoint=' + urllib.parse.quote(endpoint, safe='')
                        glob('query-propagated', 'flat/[ab].csv' + suffix, ['s3://bucket/flat/' + name + suffix for name in ['a.csv', 'b.csv']])
                        glob('literal-unknown-option', 'data.csv?unknown=1', ['s3://bucket/data.csv?unknown=1'])
                        glob('literal-bad-boolean', 'data.csv?s3_use_ssl=wrong', ['s3://bucket/data.csv?s3_use_ssl=wrong'])
                        query('wildcard-bad-boolean', "SELECT file FROM glob('s3://bucket/flat/*.csv?s3_use_ssl=wrong')", success=False)
                        query('missing-ca-setting', f"SET ca_cert_file='{quote(out / 'missing-ca.pem')}'")
                        glob('literal-no-connection', 'data.csv', ['s3://bucket/data.csv'])
                        query('reset-ca', 'RESET ca_cert_file')
                    elif kind == 'glob-errors':
                        for prefix in ['denied', 'invalid-region', 'missing-bucket', 'truncated-error', 'partial']:
                            query(prefix, f"SELECT count(*) FROM glob('s3://bucket/{prefix}/*.csv')", success=False)
                            query(prefix + '-database-valid', 'SELECT 42', [['42']])
                    elif kind == 'glob-limit':
                        query('limit', "SELECT file FROM glob('s3://bucket/partial/*.csv') LIMIT 1", [['s3://bucket/partial/00.csv']])
                        query('zero', "SELECT file FROM glob('s3://bucket/partial/*.csv') LIMIT 0", [])
                        query('prepare', "PREPARE listing AS SELECT file FROM glob('s3://bucket/partial/*.csv') LIMIT 1")
                        query('execute-one', 'EXECUTE listing', [['s3://bucket/partial/00.csv']])
                        query('execute-two', 'EXECUTE listing', [['s3://bucket/partial/00.csv']])
                        patterns = "['s3://bucket/flat/[ab].csv','s3://bucket/denied/*.csv']"
                        query('pattern-list-limit', f'SELECT file FROM glob({patterns}) LIMIT 1', [['s3://bucket/flat/a.csv']])
                        query('pattern-list-count', f'SELECT count(*) FROM glob({patterns})', success=False)
                    else:
                        query('csv-glob', "SELECT sum(i),count(*) FROM read_csv('s3://bucket/flat/[ab].csv')", [['6', '4']])
                        query('parquet-glob', "SELECT sum(i),count(*) FROM read_parquet('s3://bucket/parquet/**/*.parquet')", [['9900', '200']])
                else:
                    key = 'a b+%#é.csv' if kind == 'encoded-key' else 'a*?.csv' if kind == 'compatibility' else 'data.csv'
                    query('csv', f"SELECT sum(i), count(*) FROM read_csv('{scheme}://bucket/{key}{suffix}')", [['7' if kind == 'encoded-key' else '3', '2']])
                    query('parquet', f"SELECT sum(i), count(*) FROM read_parquet('{scheme}://bucket/data.parquet{suffix}')", [['4950', '100']])
                    query('blob', f"SELECT size, octet_length(content) FROM read_blob('{scheme}://bucket/data.csv{suffix}')", [['16', '16']])
            finally:
                lib.duckdb_disconnect(ctypes.byref(con))
                lib.duckdb_close(ctypes.byref(db))
            cases.append({'case': kind, 'records': len(records) - start})
        assert all(r['authentication_valid'] for r in requests), requests
        assert any(r['method'] == 'HEAD' for r in requests)
        assert any(r['range'] for r in requests)
        assert all(r['extra_header'] is None for r in requests if r['case'] == 'http-disabled')
        if args.listing:
            assert any('delimiter=%2F' in r['path'] for r in requests if r['case'] == 'glob-dense')
            assert not any('delimiter=' in r['path'] for r in requests if r['case'] == 'glob-flat-strategy')
            assert any('continuation-token=' in r['path'] for r in requests if r['case'] == 'glob-paged')
        (out / 'verification.json').write_text(json.dumps({'cases': cases, 'records': records,
            'requests': requests, 'native_identity': native_identity,
            'scope': 'S3 positional reads/config secrets and optional paginated/hierarchical listing against independent loopback SigV4 verification; not multipart/AWS/full release acceptance.'}, indent=2) + '\n')
        print('S3_READ_CONTROLS_PASS', len(cases), len(records), flush=True)
    finally:
        for key, value in saved_environment.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        server.shutdown()
        server.server_close()
        thread.join()
        (out / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')


if __name__ == '__main__':
    main()
