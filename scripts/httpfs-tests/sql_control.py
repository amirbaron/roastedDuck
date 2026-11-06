"""Public-C-API HTTPFS SQL activation, TLS, ETag and scoped-secret controls.

Caller authenticates the selected library and native package/probe fixtures before
and after. TLS uses synthetic cert/key files; Parquet data comes from independent
C++. Native C++ HTTPFS bytes identify the pinned Rust implementation, not a C++ ABI
emulation. This suite does not prove S3, HuggingFace or full release acceptance.
"""
import argparse
import ctypes
import datetime
import hashlib
import json
from pathlib import Path

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    parser.add_argument('httpfs_package', type=Path, help='directory containing authenticated httpfs.duckdb_extension')
    parser.add_argument('probe', type=Path)
    parser.add_argument('tls', type=Path)
    parser.add_argument('parquet', type=Path, help='independent C++ data.parquet,a.parquet,b.parquet directory')
    parser.add_argument('platform')
    parser.add_argument('out', type=Path)
    options = parser.parse_args()
    library = options.library.resolve()
    fixture = options.httpfs_package.resolve()
    probe = options.probe.resolve()
    tls = options.tls.resolve()
    platform = options.platform
    out = options.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    parquet = options.parquet / 'data.parquet'
    parquet_a = options.parquet / 'a.parquet'
    parquet_b = options.parquet / 'b.parquet'

    def sha(p):
        with p.open('rb') as f:
            return hashlib.file_digest(f, 'sha256').hexdigest()
    lib = ctypes.CDLL(str(library))
    vp = ctypes.c_void_p
    pp = ctypes.POINTER(vp)

    class Result(ctypes.Structure):
        _fields_ = [('cols', ctypes.c_uint64), ('rows', ctypes.c_uint64), ('changed', ctypes.c_uint64), ('columns', vp), ('error', vp), ('internal', vp)]
    lib.duckdb_library_version.restype = ctypes.c_char_p
    assert lib.duckdb_library_version() == b'v1.5.6'
    for name, args, restype in [('duckdb_create_config', [pp], ctypes.c_int), ('duckdb_set_config', [vp, ctypes.c_char_p, ctypes.c_char_p], ctypes.c_int), ('duckdb_open_ext', [ctypes.c_char_p, pp, vp, pp], ctypes.c_int), ('duckdb_connect', [vp, pp], ctypes.c_int), ('duckdb_query', [vp, ctypes.c_char_p, ctypes.POINTER(Result)], ctypes.c_int), ('duckdb_column_count', [ctypes.POINTER(Result)], ctypes.c_uint64), ('duckdb_row_count', [ctypes.POINTER(Result)], ctypes.c_uint64), ('duckdb_result_error', [ctypes.POINTER(Result)], ctypes.c_char_p), ('duckdb_value_varchar', [ctypes.POINTER(Result), ctypes.c_uint64, ctypes.c_uint64], vp)]:
        f = getattr(lib, name)
        f.argtypes = args
        f.restype = restype
    lib.duckdb_free.argtypes = [vp]
    lib.duckdb_destroy_result.argtypes = [ctypes.POINTER(Result)]
    lib.duckdb_destroy_config.argtypes = [pp]
    lib.duckdb_disconnect.argtypes = [pp]
    lib.duckdb_close.argtypes = [pp]
    records = []

    def query(label, sql, expect_success=True):
        r = Result()
        code = lib.duckdb_query(con, sql.encode(), ctypes.byref(r))
        error = lib.duckdb_result_error(ctypes.byref(r))
        data = []
        if code == 0:
            for row in range(lib.duckdb_row_count(ctypes.byref(r))):
                values = []
                for col in range(lib.duckdb_column_count(ctypes.byref(r))):
                    value = lib.duckdb_value_varchar(ctypes.byref(r), col, row)
                    values.append(ctypes.string_at(value).decode() if value else None)
                    if value:
                        lib.duckdb_free(value)
                data.append(values)
        record = {'label': label, 'sql': sql, 'status': code, 'error': error.decode() if error else None, 'rows': data}
        records.append(record)
        lib.duckdb_destroy_result(ctypes.byref(r))
        (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        if expect_success:
            assert code == 0, record
        return record
    quote = lambda path: str(path).replace("'", "''")
    import ssl, http.server, threading, urllib.parse, re, gzip
    records = []
    requests = []
    changed = set()
    raw_probe = (probe / 'raw-v1/c_only_probe.duckdb_extension').read_bytes()
    data = {'data.csv': b'i,s\n1,one\n2,two\n', 'data.parquet': parquet.read_bytes(), 'c_only_probe.duckdb_extension.gz': gzip.compress(raw_probe, mtime=0)}

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *args):
            pass

        def do_HEAD(self):
            self.respond(False)

        def do_GET(self):
            self.respond(True)

        def respond(self, body):
            path = urllib.parse.urlsplit(self.path).path
            kind = path.split('/')[1]
            name = path.rsplit('/', 1)[-1]
            requests.append({'kind': kind, 'method': self.command, 'path': self.path, 'range': self.headers.get('Range'), 'authorization': self.headers.get('Authorization'), 'fixture_key': self.headers.get('X-Fixture-Key')})
            payload = data.get(name)
            if kind == 'etag' and kind in changed:
                payload = b'i,s\n3,one\n4,two\n'
            if kind == 'etag-parquet':
                payload = (parquet_b if kind in changed else parquet_a).read_bytes()
            if kind == 'switch':
                payload = b'ok'
            auth_ok = kind not in ['secret-bearer', 'secret-scope-negative', 'secret-redirect'] or self.headers.get('Authorization') == 'Bearer fixture-token'
            auth_ok = auth_ok and (kind != 'secret-extra' or self.headers.get('X-Fixture-Key') == 'fixture-header')
            status = 200
            start = 0
            end = len(payload) - 1 if payload is not None else 0
            if auth_ok and name == 'redirect.csv':
                self.send_response(302)
                self.send_header('Location', '/' + kind + '/data.csv')
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            if not auth_ok:
                payload = b'not authorized'
                status = 401
            elif payload is None:
                payload = b'missing'
                status = 404
            elif body and self.headers.get('Range'):
                match = re.fullmatch('bytes=(\\d+)-(\\d+)', self.headers['Range'])
                assert match, self.headers['Range']
                start = int(match[1])
                end = min(int(match[2]), len(payload) - 1)
                status = 206
            self.send_response(status)
            self.send_header('Content-Length', str(end - start + 1 if status == 206 else len(payload)))
            self.send_header('ETag', '"fixture-v2"' if kind in changed else '"fixture-v1"')
            self.send_header('Last-Modified', 'Wed, 01 Jan 2020 00:00:00 GMT')
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(payload)}')
            self.end_headers()
            if body:
                self.wfile.write(payload[start:end + 1] if status == 206 else payload)
                if kind == 'etag':
                    changed.add(kind)
                if kind == 'switch':
                    changed.add(name)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(tls / 'server-cert.pem', tls / 'server-key.pem')
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    origin = 'https://localhost:' + str(server.server_port)
    cases = []
    try:
        for kind in ['trusted', 'untrusted', 'disabled-verification', 'httplib-default', 'httplib-strict', 'redirect', 'parquet', 'install', 'missing', 'etag', 'etag-parquet', 'secret-bearer', 'secret-extra', 'secret-scope-negative', 'secret-redirect', 'secret-tls-override']:
            case = out / kind
            case.mkdir()
            directory = case / 'extensions'
            start_record = len(records)
            config = vp()
            db = vp()
            con = vp()
            err = vp()
            assert lib.duckdb_create_config(ctypes.byref(config)) == 0
            for key, value in [(b'allow_unsigned_extensions', b'true' if kind == 'install' else b'false'), (b'allow_community_extensions', b'false'), (b'autoinstall_known_extensions', b'false'), (b'autoload_known_extensions', b'false'), (b'extension_directory', str(directory).encode()), (b'secret_directory', str(case / 'secrets').encode())]:
                assert lib.duckdb_set_config(config, key, value) == 0
            assert lib.duckdb_open_ext(None, ctypes.byref(db), config, ctypes.byref(err)) == 0
            lib.duckdb_destroy_config(ctypes.byref(config))
            assert lib.duckdb_connect(db, ctypes.byref(con)) == 0
            query(kind + '-install-httpfs', "INSTALL '" + quote(fixture / 'httpfs.duckdb_extension') + "'")
            query(kind + '-load-httpfs', 'LOAD httpfs')
            query(kind + '-repeat-load', 'LOAD httpfs')
            query(kind + '-retry', 'SET http_retries=0')
            if kind not in ['untrusted', 'disabled-verification', 'httplib-default', 'httplib-strict', 'secret-tls-override']:
                query(kind + '-ca', "SET ca_cert_file='" + quote(tls / 'server-cert.pem') + "'")
            if kind == 'disabled-verification':
                query(kind + '-disable-cert', 'SET enable_curl_server_cert_verification=false')
            if kind.startswith('httplib-'):
                query(kind + '-client', "SET httpfs_client_implementation='httplib'")
            if kind == 'httplib-strict':
                query(kind + '-enable-cert', 'SET enable_server_cert_verification=true')
            if kind.startswith('secret-'):
                scope = origin + ('/other/' if kind == 'secret-scope-negative' else '/' + kind + '/')
                option = "EXTRA_HTTP_HEADERS MAP {'X-Fixture-Key':'fixture-header'}" if kind == 'secret-extra' else 'VERIFY_SSL false' if kind == 'secret-tls-override' else "BEARER_TOKEN 'fixture-token'"
                query(kind + '-secret', 'CREATE SECRET fixture_http (TYPE HTTP, ' + option + ", SCOPE ['" + scope + "'])")
            if kind == 'install':
                result = query(kind + '-query', "INSTALL '" + origin + '/' + kind + "/c_only_probe.duckdb_extension.gz'")
                installed = directory / 'v1.5.6' / platform / 'c_only_probe.duckdb_extension'
                assert installed.read_bytes() == raw_probe
                query(kind + '-load', 'LOAD c_only_probe')
                assert query(kind + '-function', 'SELECT c_only_add7(5)')['rows'] == [['12']]
            else:
                name = 'data.parquet' if kind in ['parquet', 'etag-parquet'] else 'redirect.csv' if kind in ['redirect', 'secret-redirect'] else 'missing.csv' if kind == 'missing' else 'data.csv'
                url = origin + '/' + kind + '/' + name
                sql = "SELECT sum(i),count(s) FROM read_parquet('" + url + "')" if kind in ['parquet', 'etag-parquet'] else "SELECT sum(i),string_agg(s,',' ORDER BY i) FROM read_csv_auto('" + url + "')"
                ok = kind not in ['untrusted', 'httplib-strict', 'missing', 'secret-scope-negative']
                result = query(kind + '-query', sql, ok)
                if ok:
                    assert result['rows'] == ([['4950', '100']] if kind in ['parquet', 'etag-parquet'] else [['3', 'one,two']])
                else:
                    assert result['status'] != 0
            if kind == 'etag':
                assert query(kind + '-second-query', sql)['rows'] == [['7', 'one,two']]
            if kind == 'etag-parquet':
                query(kind + '-switch', "SELECT octet_length(content) FROM read_blob('" + origin + "/switch/etag-parquet')")
                assert query(kind + '-second-query', sql + ' WHERE i>=100')['rows'] == [['14950', '100']]
            assert query(kind + '-database-valid', 'SELECT 42')['rows'] == [['42']]
            lib.duckdb_disconnect(ctypes.byref(con))
            lib.duckdb_close(ctypes.byref(db))
            cases.append({'kind': kind, 'records': records[start_record:], 'requests': [r for r in requests if r['kind'] == kind]})
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    (out / 'verification.json').write_text(json.dumps({'cases': cases, 'origin': origin, 'scope': 'Public C API SQL activation and HTTP(S)/scoped-secret controls; caller authenticates native inputs. Not full HTTPFS, client suites, platform or release acceptance.'}, indent=2) + '\n')
    print('SIXTEEN_SQL_TLS_SECRET_CONTROLS_PASS', flush=True)
if __name__ == '__main__':
    main()
