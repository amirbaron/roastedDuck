"""Public C API partitioned S3 COPY and bulk-delete controls against an independent receiver.

Caller authenticates the engine and HTTPFS package before/after. The receiver
verifies SigV4 including actual payload hashes, stages parts on the external test
directory, and publishes only after validating the completion manifest. Synthetic
credentials and loopback HTTP only. This is not AWS interoperability acceptance.
"""
import argparse
import base64
import ctypes
import gzip
import hashlib
import hmac
import http.server
import json
import os
import re
import shutil
import subprocess
import tempfile
import threading
import urllib.parse
import xml.etree.ElementTree as ET
from pathlib import Path


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('library', type=Path)
    p.add_argument('package', type=Path)
    p.add_argument('out', type=Path)
    p.add_argument('--sdk-driver', type=Path, help='caller-authenticated native SDK removal driver')
    args = p.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    for name in ['objects', 'parts', 'requests']:
        (out / name).mkdir()
    lib = ctypes.CDLL(str(args.library.resolve()))
    vp = ctypes.c_void_p
    pp = ctypes.POINTER(vp)

    class Result(ctypes.Structure):
        _fields_ = [('cols', ctypes.c_uint64), ('rows', ctypes.c_uint64), ('changed', ctypes.c_uint64),
                    ('columns', vp), ('error', vp), ('internal', vp)]

    lib.duckdb_library_version.restype = ctypes.c_char_p
    assert lib.duckdb_library_version() == b'v1.5.6'
    for name, params, result in [
        ('duckdb_create_config', [pp], ctypes.c_int), ('duckdb_set_config', [vp, ctypes.c_char_p, ctypes.c_char_p], ctypes.c_int),
        ('duckdb_open_ext', [ctypes.c_char_p, pp, vp, pp], ctypes.c_int), ('duckdb_connect', [vp, pp], ctypes.c_int),
        ('duckdb_query', [vp, ctypes.c_char_p, ctypes.POINTER(Result)], ctypes.c_int),
        ('duckdb_result_error', [ctypes.POINTER(Result)], ctypes.c_char_p),
        ('duckdb_column_count', [ctypes.POINTER(Result)], ctypes.c_uint64), ('duckdb_row_count', [ctypes.POINTER(Result)], ctypes.c_uint64),
        ('duckdb_value_is_null', [ctypes.POINTER(Result), ctypes.c_uint64, ctypes.c_uint64], ctypes.c_bool),
        ('duckdb_value_varchar', [ctypes.POINTER(Result), ctypes.c_uint64, ctypes.c_uint64], vp),
    ]:
        function = getattr(lib, name)
        function.argtypes, function.restype = params, result
    lib.duckdb_destroy_result.argtypes = [ctypes.POINTER(Result)]
    lib.duckdb_free.argtypes = [vp]
    for name in ['duckdb_destroy_config', 'duckdb_disconnect', 'duckdb_close']:
        getattr(lib, name).argtypes = [pp]
    records, requests = [], []
    visible, uploads, attempts = {}, {}, {}
    lock = threading.Lock()
    blocked, resume = threading.Event(), threading.Event()
    memory_samples = []

    def query(con, label, sql, expected=None, success=True):
        result = Result()
        status = lib.duckdb_query(con, sql.encode(), ctypes.byref(result))
        error = lib.duckdb_result_error(ctypes.byref(result))
        rows = []
        if status == 0:
            for row in range(lib.duckdb_row_count(ctypes.byref(result))):
                values = []
                for column in range(lib.duckdb_column_count(ctypes.byref(result))):
                    if lib.duckdb_value_is_null(ctypes.byref(result), column, row):
                        values.append(None)
                    else:
                        ptr = lib.duckdb_value_varchar(ctypes.byref(result), column, row)
                        values.append(ctypes.string_at(ptr).decode())
                        lib.duckdb_free(ptr)
                rows.append(values)
        record = {'label': label, 'sql': sql, 'status': status, 'error': error.decode() if error else None, 'rows': rows}
        records.append(record)
        lib.duckdb_destroy_result(ctypes.byref(result))
        (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        assert (status == 0) == success, record
        if expected is not None:
            assert rows == expected, record
        return record

    def authenticated(handler, digest):
        auth = handler.headers.get('Authorization', '')
        match = re.fullmatch(r'AWS4-HMAC-SHA256 Credential=fixture-access/([^/]+)/fixture-region/s3/aws4_request, SignedHeaders=([^,]+), Signature=([0-9a-f]{64})', auth)
        if not match:
            return False
        date, signed, signature = match.groups()
        names = signed.split(';')
        if names != sorted(names) or handler.headers.get('x-amz-content-sha256') != digest:
            return False
        datetime = handler.headers.get('x-amz-date', '')
        if not re.fullmatch(date + r'T\d{6}Z', datetime):
            return False
        parsed = urllib.parse.urlsplit(handler.path)
        canonical_headers = ''.join(name + ':' + ' '.join(handler.headers.get(name, '').split()) + '\n' for name in names)
        canonical_query = 'delete=' if parsed.query == 'delete' else parsed.query
        canonical = '\n'.join([handler.command, parsed.path, canonical_query, canonical_headers, signed, digest])
        scope = date + '/fixture-region/s3/aws4_request'
        message = '\n'.join(['AWS4-HMAC-SHA256', datetime, scope, hashlib.sha256(canonical.encode()).hexdigest()])
        key = b'AWS4fixture-secret'
        for value in [date, 'fixture-region', 's3', 'aws4_request']:
            key = hmac.new(key, value.encode(), hashlib.sha256).digest()
        return hmac.compare_digest(signature, hmac.new(key, message.encode(), hashlib.sha256).hexdigest())

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *unused):
            pass

        def do_HEAD(self):
            self.respond()

        def do_GET(self):
            self.respond()

        def do_POST(self):
            self.respond()

        def do_PUT(self):
            self.respond()

        def do_DELETE(self):
            self.respond()

        def respond(self):
            parsed = urllib.parse.urlsplit(self.path)
            key = urllib.parse.unquote(parsed.path).removeprefix('/bucket/')
            params = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
            length = int(self.headers.get('Content-Length', '0'))
            sha, md5 = hashlib.sha256(), hashlib.md5()
            temporary = None
            if length:
                with tempfile.NamedTemporaryFile(dir=out / 'parts', delete=False) as file:
                    temporary = Path(file.name)
                    remaining = length
                    while remaining:
                        chunk = self.rfile.read(min(remaining, 65536))
                        assert chunk, 'Short request payload'
                        sha.update(chunk)
                        md5.update(chunk)
                        file.write(chunk)
                        remaining -= len(chunk)
            digest = sha.hexdigest()
            valid = authenticated(self, digest)
            record = {'method': self.command, 'path': self.path, 'key': key, 'size': length,
                      'sha256': digest, 'authentication_valid': valid, 'range': self.headers.get('Range'),
                      'kms': self.headers.get('x-amz-server-side-encryption'),
                      'kms_key': self.headers.get('x-amz-server-side-encryption-aws-kms-key-id')}
            status, response, etag, file = 200, b'', None, None
            with lock:
                requests.append(record)
                identity = (key, self.command, parsed.query)
                attempt = attempts.get(identity, 0)
                attempts[identity] = attempt + 1
            if not valid:
                status, response = 403, b'<Error><Code>SignatureDoesNotMatch</Code><Message>fixture signature failure</Message></Error>'
            elif self.command == 'GET' and params.get('list-type') == ['2']:
                assert parsed.path == '/bucket/' and params.get('encoding-type') == ['url'], self.path
                prefix = params['prefix'][0]
                delimiter = params.get('delimiter', [''])[0]
                assert not delimiter, params
                with lock:
                    keys = sorted(key for key in visible if key.startswith(prefix))
                offset = int(params.get('continuation-token', ['0'])[0])
                selected = keys[offset:offset + 128]
                xml = ['<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">']
                for key in selected:
                    path, object_etag = visible[key]
                    xml.append('<Contents><Key>' + urllib.parse.quote(key, safe='') + '</Key><LastModified>2020-01-01T00:00:00.000Z</LastModified><ETag>' + object_etag.replace('"', '&quot;') + '</ETag><Size>' + str(path.stat().st_size) + '</Size></Contents>')
                if offset + 128 < len(keys):
                    xml.append('<NextContinuationToken>' + str(offset + 128) + '</NextContinuationToken>')
                xml.append('</ListBucketResult>')
                response = ''.join(xml).encode()
            elif self.command == 'POST' and 'delete' in params:
                assert parsed.path == '/bucket/' and len(params) == 1, self.path
                assert self.headers.get('Content-Type') == 'application/xml'
                assert self.headers.get('Content-MD5') == base64.b64encode(md5.digest()).decode(), 'Incorrect Content-MD5'
                root = ET.parse(temporary).getroot()
                keys = [node.text for node in root.findall('{*}Object/{*}Key')]
                assert 0 < len(keys) <= 1000, len(keys)
                record['delete_keys'] = keys
                if any(key.startswith('delete-body-error/') for key in keys):
                    response = b'<DeleteResult><Error><Key>delete-body-error/old.csv</Key><Code>AccessDenied</Code><Message>fixture delete failure</Message></Error></DeleteResult>'
                elif any(key.startswith('delete-http-error/') for key in keys):
                    status, response = 403, b'<Error><Code>AccessDenied</Code><Message>fixture HTTP delete failure</Message></Error>'
                else:
                    with lock:
                        for key in keys:
                            visible.pop(key, None)
                    response = b'<DeleteResult/>'
            elif self.command == 'DELETE':
                if key == 'deny-single.csv':
                    status, response = 403, b'<Error><Code>AccessDenied</Code><Message>fixture single delete failure</Message></Error>'
                else:
                    with lock:
                        existed = visible.pop(key, None)
                    status = 204 if existed is not None else 404
            elif self.command == 'POST' and 'uploads' in params:
                if key == 'init-error.csv':
                    status, response = 403, b'<Error><Code>AccessDenied</Code><Message>fixture init failure</Message></Error>'
                else:
                    with lock:
                        upload_id = 'fixture/upload-' + str(len(uploads) + 1) + '+id='
                        uploads[upload_id] = {'key': key, 'parts': {}, 'complete': False}
                    response = ('<InitiateMultipartUploadResult><UploadId>' + upload_id + '</UploadId></InitiateMultipartUploadResult>').encode()
            elif self.command == 'PUT':
                part = int(params['partNumber'][0]) if 'partNumber' in params else None
                if key == 'part-error.csv' and part == 2:
                    status, response = 403, b'<Error><Code>AccessDenied</Code><Message>fixture part failure</Message></Error>'
                elif key in ['part-retry.csv', 'part-timeout.csv'] and part == 1 and attempt == 0:
                    status = 400 if key == 'part-timeout.csv' else 503
                    code = 'RequestTimeout' if status == 400 else 'SlowDown'
                    response = ('<Error><Code>' + code + '</Code><Message>fixture transient</Message></Error>').encode()
                else:
                    assert temporary is not None
                    etag = '"' + md5.hexdigest() + '"'
                    if part is None:
                        file = out / 'objects' / hashlib.sha256(key.encode()).hexdigest()
                        file.parent.mkdir(parents=True, exist_ok=True)
                        os.replace(temporary, file)
                        with lock:
                            visible[key] = (file, etag)
                    else:
                        upload_id = params['uploadId'][0]
                        with lock:
                            upload = uploads[upload_id]
                            assert upload['key'] == key and not upload['complete']
                            upload['parts'][part] = (temporary, etag)
                        if key == 'accounted.csv' and part == 1:
                            blocked.set()
                            assert resume.wait(60), 'Memory-accounting observer did not release PUT'
                    if key == 'missing-etag.csv':
                        etag = None
            elif self.command == 'POST' and 'uploadId' in params:
                upload_id = params['uploadId'][0]
                if key == 'complete-error.csv' or key == 'complete-retry.csv' and attempt == 0:
                    code = 'InternalError' if key == 'complete-retry.csv' else 'InvalidPart'
                    response = ('<Error><Code>' + code + '</Code><Message>fixture completion failure</Message></Error>').encode()
                else:
                    assert temporary is not None
                    manifest = ET.parse(temporary).getroot()
                    parts = [(int(item.findtext('{*}PartNumber')), item.findtext('{*}ETag')) for item in manifest.findall('{*}Part')]
                    assert parts and parts == sorted(parts) and [n for n, _ in parts] == list(range(1, len(parts) + 1)), parts
                    file = out / 'objects' / hashlib.sha256(key.encode()).hexdigest()
                    file.parent.mkdir(parents=True, exist_ok=True)
                    with lock:
                        upload = uploads[upload_id]
                        assert upload['key'] == key and not upload['complete']
                        with file.open('wb') as target:
                            for part, expected in parts:
                                source, actual = upload['parts'][part]
                                assert actual == expected, (actual, expected)
                                with source.open('rb') as chunk:
                                    shutil.copyfileobj(chunk, target, 65536)
                        upload['complete'] = True
                        etag = '"fixture-multipart-' + str(len(parts)) + '"'
                        visible[key] = (file, etag)
                    response = b'<CompleteMultipartUploadResult><ETag>"fixture-complete"</ETag></CompleteMultipartUploadResult>'
            elif self.command in ['GET', 'HEAD']:
                with lock:
                    value = visible.get(key)
                if value is None:
                    status, response = 404, b'<Error><Code>NoSuchKey</Code><Message>fixture missing key</Message></Error>'
                else:
                    file, etag = value
            else:
                status, response = 400, b'Unexpected fixture operation'
            if self.command not in ['GET', 'HEAD']:
                file = None
            start, end = 0, file.stat().st_size - 1 if file is not None else len(response) - 1
            if self.command in ['GET', 'HEAD'] and file is not None and self.headers.get('Range'):
                match = re.fullmatch(r'bytes=(\d+)-(\d+)', self.headers['Range'])
                assert match
                start, end = int(match[1]), min(int(match[2]), end)
                status = 206
            record['status'] = status
            self.send_response(status)
            self.send_header('Content-Length', str(end - start + 1))
            self.send_header('Last-Modified', 'Wed, 01 Jan 2020 00:00:00 GMT')
            if etag is not None:
                self.send_header('ETag', etag)
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{file.stat().st_size}')
            self.end_headers()
            if self.command != 'HEAD':
                if file is not None and self.command == 'GET':
                    with file.open('rb') as source:
                        source.seek(start)
                        remaining = end - start + 1
                        while remaining:
                            chunk = source.read(min(remaining, 65536))
                            self.wfile.write(chunk)
                            remaining -= len(chunk)
                else:
                    self.wfile.write(response)

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    endpoint = '127.0.0.1:' + str(server.server_port)
    quote = lambda value: str(value).replace("'", "''")
    db, con, config, error = vp(), vp(), vp(), vp()
    try:
        assert lib.duckdb_create_config(ctypes.byref(config)) == 0
        for key, value in [('allow_unsigned_extensions', 'false'), ('allow_community_extensions', 'false'),
                           ('autoinstall_known_extensions', 'false'), ('autoload_known_extensions', 'false'),
                           ('secret_directory', str(out / 'secrets'))]:
            assert lib.duckdb_set_config(config, key.encode(), value.encode()) == 0
        assert lib.duckdb_open_ext(None, ctypes.byref(db), config, ctypes.byref(error)) == 0
        lib.duckdb_destroy_config(ctypes.byref(config))
        assert lib.duckdb_connect(db, ctypes.byref(con)) == 0
        query(con, 'load', f"LOAD '{quote(args.package / 'httpfs.duckdb_extension')}'")
        query(con, 'secret', f"CREATE SECRET credentials (TYPE s3, KEY_ID 'fixture-access', SECRET 'fixture-secret', REGION 'fixture-region', ENDPOINT '{endpoint}', URL_STYLE 'path', USE_SSL false)")
        for name, value in [('s3_uploader_max_filesize', "'50MB'"), ('s3_uploader_thread_limit', '2'),
                            ('http_retries', '1'), ('http_retry_wait_ms', '1'), ('threads', '2')]:
            query(con, name, f'SET {name}={value}')
        def seed(key, data=b'i\n99\n'):
            path = out / 'objects' / hashlib.sha256(key.encode()).hexdigest()
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            visible[key] = (path, '"' + hashlib.md5(data).hexdigest() + '"')

        def partition(label, select, root, options='', success=True, format='CSV'):
            return query(con, label, f"COPY ({select}) TO 's3://bucket/{root}' (FORMAT {format}, PARTITION_BY(p){options})", success=success)

        def read(label, root, expected, format='csv'):
            return query(con, label, f"SELECT sum(i),count(*) FROM read_{format}('s3://bucket/{root}/**/*.{format}',hive_partitioning=true)", [expected])

        partition('partition-csv', 'SELECT i,i%3 p FROM range(12)t(i)', 'part')
        read('partition-csv-read', 'part', ['66','12'])
        query(con, 'partition-csv-types', "SELECT typeof(p),count(*) FROM read_csv('s3://bucket/part/**/*.csv',hive_partitioning=true) GROUP BY typeof(p)", [['BIGINT','12']])
        before = {key:value for key,value in visible.items() if key.startswith('part/')}
        partition('existing-directory', 'SELECT i,i%3 p FROM range(6)t(i)', 'part', success=False)
        assert before == {key:value for key,value in visible.items() if key.startswith('part/')}
        partition('overwrite-ignore', 'SELECT i,i%3 p FROM range(100,106)t(i)', 'part', ',OVERWRITE_OR_IGNORE true')
        read('overwrite-ignore-read', 'part', ['615','6'])
        seed('part/obsolete.csv')
        query(con, 'scoped-secret', f"CREATE SECRET one (TYPE s3, KEY_ID 'fixture-access', SECRET 'fixture-secret', REGION 'fixture-region', ENDPOINT '{endpoint}', URL_STYLE 'path', USE_SSL false, SCOPE 's3://bucket/part/p=1/')")
        partition('overwrite', 'SELECT i,i%3 p FROM range(20,26)t(i)', 'part', ',OVERWRITE true')
        assert 'part/obsolete.csv' not in visible
        read('overwrite-read', 'part', ['135','6'])
        part_deletes = [r['delete_keys'] for r in requests if 'delete_keys' in r and any(k.startswith('part/') for k in r['delete_keys'])]
        assert len(part_deletes) == 2 and sum(map(len, part_deletes)) == 4, part_deletes
        assert any(keys == ['part/p=1/data_0.csv'] for keys in part_deletes), part_deletes
        partition('append', 'SELECT i,i%3 p FROM range(30,36)t(i)', 'part', ',APPEND true')
        read('append-read', 'part', ['330','12'])
        assert len([key for key in visible if key.startswith('part/')]) == 6
        partition('parquet', 'SELECT i,i%2 p FROM range(20)t(i)', 'parquet', format='PARQUET')
        read('parquet-read', 'parquet', ['190','20'], format='parquet')
        select = "SELECT i,CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN 'a b' ELSE 'é/plus+%?' END p FROM range(6)t(i)"
        partition('encoded-null', select, 'encoded')
        query(con, 'encoded-null-read', "SELECT p,count(*) FROM read_csv('s3://bucket/encoded/**/*.csv',hive_partitioning=true) GROUP BY p ORDER BY p NULLS LAST", [['a b','2'],['é/plus+%?','2'],[None,'2']])
        seed('coexist')
        partition('remote-file-prefix-coexist', 'SELECT i,i%2 p FROM range(4)t(i)', 'coexist')
        assert visible['coexist'][0].read_bytes() == b'i\n99\n'
        read('coexist-read', 'coexist', ['6','4'])
        for n in range(1005):
            seed(f'bulk/old_{n:04}.csv')
        partition('bulk-overwrite', 'SELECT 7 i,0 p', 'bulk', ',OVERWRITE true')
        bulk_deletes = [r['delete_keys'] for r in requests if 'delete_keys' in r and any(k.startswith('bulk/') for k in r['delete_keys'])]
        assert sorted(map(len,bulk_deletes)) == [5,1000], list(map(len,bulk_deletes))
        assert len([key for key in visible if key.startswith('bulk/')]) == 1
        read('bulk-read', 'bulk', ['7','1'])
        for prefix in ['delete-body-error','delete-http-error']:
            seed(prefix+'/old.csv')
            partition(prefix, 'SELECT 7 i,0 p', prefix, ',OVERWRITE true', success=False)
            assert [key for key in visible if key.startswith(prefix+'/')] == [prefix+'/old.csv']
            query(con,prefix+'-database-valid','SELECT 42',[['42']])
        sdk_records = []
        if args.sdk_driver:
            sdk_env = {key: value for key, value in os.environ.items() if not key.startswith(('AWS_', 'DUCKDB_S3_')) and key.lower() not in ['http_proxy', 'https_proxy', 'all_proxy']}
            with (out / 'sdk.log').open('x') as log:
                child = subprocess.run([str(args.sdk_driver), endpoint, str(args.package / 'httpfs.duckdb_extension'), str(out / 'sdk-records.json')], env=sdk_env, stdout=log, stderr=subprocess.STDOUT)
            assert child.returncode == 0, (out / 'sdk.log').read_text()
            sdk_records = json.loads((out / 'sdk-records.json').read_text())
            assert len(sdk_records) == 11
            assert 'delete-single.csv' not in visible and 'deny-single.csv' in visible
        assert all(r['authentication_valid'] for r in requests), requests
        objects = {key: {'bytes': file.stat().st_size, 'sha256': hashlib.file_digest(file.open('rb'), 'sha256').hexdigest()} for key, (file, _) in visible.items()}
        (out / 'verification.json').write_text(json.dumps({'records': records, 'sdk_records': sdk_records, 'requests': requests, 'objects': objects,
            'memory_samples': memory_samples, 'uploads': {key: {'object': value['key'], 'parts': len(value['parts']), 'complete': value['complete']} for key, value in uploads.items()},
            'scope': 'Loopback partitioned COPY/overwrite/append/bulk delete/scoped batching/error controls only; no full S3 or release acceptance.'}, indent=2) + '\n')
        print('S3_PARTITION_CONTROLS_PASS', len(records), flush=True)
    finally:
        resume.set()
        lib.duckdb_disconnect(ctypes.byref(con))
        lib.duckdb_close(ctypes.byref(db))
        server.shutdown()
        server.server_close()
        thread.join()
        (out / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')


if __name__ == '__main__':
    main()
