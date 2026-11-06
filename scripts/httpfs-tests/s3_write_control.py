"""Public C API S3 COPY controls against an independent on-disk S3 receiver.

Caller authenticates the engine and HTTPFS package before/after. The receiver
verifies SigV4 including actual payload hashes, stages parts on the external test
directory, and publishes only after validating the completion manifest. Synthetic
credentials and loopback HTTP only. This is not AWS interoperability acceptance.
"""
import argparse
import ctypes
import gzip
import hashlib
import hmac
import http.server
import json
import os
import re
import shutil
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
        canonical = '\n'.join([handler.command, parsed.path, parsed.query, canonical_headers, signed, digest])
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
                        file = out / 'objects' / key
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
                    file = out / 'objects' / key
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
        small = "SELECT i, 'row'||i::VARCHAR s FROM range(3)t(i)"
        large = "SELECT i, repeat('x',128) s FROM range(100000)t(i)"

        def copy(label, select, name, format='CSV', success=True):
            return query(con, label, f"COPY ({select}) TO 's3://bucket/{name}' (FORMAT {format})", success=success)

        copy('csv-small', small, 'small.csv')
        assert visible['small.csv'][0].read_bytes() == b'i,s\n0,row0\n1,row1\n2,row2\n'
        query(con, 'csv-readback', "SELECT sum(i),count(*) FROM read_csv('s3://bucket/small.csv')", [['3', '3']])
        copy('blob-small', "SELECT 'fixture payload'::BLOB", 'small.blob', 'BLOB')
        assert visible['small.blob'][0].read_bytes() == b'fixture payload'
        copy('csv-gzip', small, 'small.csv.gz')
        assert gzip.decompress(visible['small.csv.gz'][0].read_bytes()) == visible['small.csv'][0].read_bytes()
        query(con, 'gzip-readback', "SELECT sum(i),count(*) FROM read_csv('s3://bucket/small.csv.gz')", [['3', '3']])
        copy('parquet-small', "SELECT i FROM range(100)t(i)", 'small.parquet', 'PARQUET')
        query(con, 'parquet-readback', "SELECT sum(i),count(*) FROM read_parquet('s3://bucket/small.parquet')", [['4950', '100']])
        for name in ['multi.csv', 'part-retry.csv', 'part-timeout.csv', 'complete-retry.csv']:
            copy(name, large, name)
            query(con, name+'-readback', f"SELECT sum(i),count(*) FROM read_csv('s3://bucket/{name}')", [['4999950000', '100000']])
        query(con, 'kms-setting', "SET s3_kms_key_id='fixture-kms'")
        copy('kms-small', small, 'kms.csv')
        query(con, 'kms-reset', 'RESET s3_kms_key_id')
        assert any(r['key'] == 'kms.csv' and r['kms'] == 'aws:kms' and r['kms_key'] == 'fixture-kms' for r in requests)
        for name in ['init-error.csv', 'part-error.csv', 'complete-error.csv']:
            failure = copy(name, large, name, success=False)
            if name == 'part-error.csv':
                # A failed upload part must retain its original response. Losing
                # the worker's opener used to refresh against a default endpoint
                # and replace this with an unrelated NoSuchUpload error.
                assert failure['error'] == (
                    'HTTP Error: Unable to connect to URL s3://bucket/part-error.csv: '
                    'Forbidden (HTTP code 403)\n\nAccessDenied: fixture part failure'
                ), failure
            assert name not in visible, name
            query(con, name+'-valid', 'SELECT 42', [['42']])
        copy('missing-etag', small, 'missing-etag.csv', success=False)
        assert 'missing-etag.csv' in visible
        query(con, 'memory-before', "SELECT memory_usage_bytes FROM duckdb_memory() WHERE tag='EXTENSION'", [['0']])
        observer = vp()
        assert lib.duckdb_connect(db, ctypes.byref(observer)) == 0
        errors = []
        def background():
            try:
                copy('accounted-copy', large, 'accounted.csv')
            except BaseException as error:
                errors.append(str(error))
        worker = threading.Thread(target=background)
        worker.start()
        try:
            assert blocked.wait(60), 'Upload never reached memory-accounting barrier'
            r = query(observer, 'memory-during', "SELECT memory_usage_bytes FROM duckdb_memory() WHERE tag='EXTENSION'")
            charged = int(r['rows'][0][0])
            assert charged >= 5242880, charged
            memory_samples.append(charged)
            assert 'accounted.csv' not in visible
        finally:
            resume.set()
            worker.join(60)
            lib.duckdb_disconnect(ctypes.byref(observer))
        assert not worker.is_alive() and not errors, errors
        query(con, 'memory-after', "SELECT memory_usage_bytes FROM duckdb_memory() WHERE tag='EXTENSION'", [['0']])
        assert all(r['authentication_valid'] for r in requests), requests
        objects = {key: {'bytes': file.stat().st_size, 'sha256': hashlib.file_digest(file.open('rb'), 'sha256').hexdigest()} for key, (file, _) in visible.items()}
        (out / 'verification.json').write_text(json.dumps({'records': records, 'requests': requests, 'objects': objects,
            'memory_samples': memory_samples, 'uploads': {key: {'object': value['key'], 'parts': len(value['parts']), 'complete': value['complete']} for key, value in uploads.items()},
            'scope': 'Loopback COPY/part/finalization/visibility/accounting controls only; no AWS service or final release/performance acceptance.'}, indent=2) + '\n')
        print('S3_WRITE_CONTROLS_PASS', len(records), flush=True)
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
