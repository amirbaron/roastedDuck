"""Exercise HTTP extension installation through the public C API.

The caller authenticates the selected library and raw-v1/raw-v2 C-only fixtures
before and after execution. These fixtures export c_only_add7 and carry versions
0.0.1/0.0.2. The server uses only loopback and synthetic test credentials.
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
    parser.add_argument('fixtures', type=Path)
    parser.add_argument('platform')
    parser.add_argument('out', type=Path)
    args = parser.parse_args()
    library = args.library.resolve()
    fixture = args.fixtures.resolve()
    platform = args.platform
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)

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
    import http.server, threading, gzip, urllib.parse
    requests = []
    phase = {}
    counts = {}
    raw = {v: (fixture / ('raw-' + v) / 'c_only_probe.duckdb_extension').read_bytes() for v in ['v1', 'v2']}

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *args):
            pass

        def do_GET(self):
            path = urllib.parse.urlsplit(self.path).path
            kind = path.split('/')[1]
            counts[kind] = counts.get(kind, 0) + 1
            requests.append({'kind': kind, 'path': self.path, 'if_none_match': self.headers.get('If-None-Match'), 'proxy_auth': self.headers.get('Proxy-Authorization')})
            status = 200
            data = gzip.compress(raw['v2' if phase.get(kind) == 'force' else 'v1'], mtime=0)
            if kind == 'raw':
                data = raw['v1']
            if kind == 'permanent503':
                status = 503
                data = b'busy'
            if kind == 'crc':
                data = data[:-8] + bytes([data[-8] ^ 1]) + data[-7:]
            if kind == 'concat':
                data += gzip.compress(raw['v2'], mtime=0)
            if kind == 'truncated':
                data = data[:len(data) // 2]
            if kind == 'empty':
                data = gzip.compress(b'', mtime=0)
            if kind == 'extra':
                data = data[:3] + bytes([data[3] | 4]) + data[4:10] + bytes(2) + data[10:]
            if kind == 'ascii':
                data = data[:3] + bytes([data[3] | 1]) + data[4:]
            if kind == 'badgzip' or (kind == 'badforce' and phase.get(kind) == 'force'):
                data = b'bad gzip'
            if kind == '404':
                status = 404
                data = b'missing'
            if kind == '302':
                status = 302
                data = b''
            if kind == 'retry' and counts[kind] < 3:
                status = 503
                data = b'busy'
            if kind == '304empty' or (kind == '304' and phase.get(kind) == 'force'):
                status = 304
                data = b''
            self.send_response(status)
            self.send_header('ETag', '"fixture-' + ('v2' if phase.get(kind) == 'force' else 'v1') + '"')
            if status == 302:
                self.send_header('Location', '/gzip/c_only_probe.duckdb_extension.gz')
            if kind == 'chunked':
                self.send_header('Transfer-Encoding', 'chunked')
                self.end_headers()
                for offset in range(0, len(data), 97):
                    chunk = data[offset:offset + 97]
                    self.wfile.write(('%x\r\n' % len(chunk)).encode() + chunk + b'\r\n')
                self.wfile.write(b'0\r\nX-Test-Trailer: complete\r\n\r\n')
            elif kind == 'close':
                self.send_header('Connection', 'close')
                self.end_headers()
                self.wfile.write(data)
                self.close_connection = True
            else:
                self.send_header('Content-Length', str(len(data) + (100 if kind == 'shortalways' or (kind == 'shortretry' and counts[kind] == 1) else 0)))
                self.send_header('Connection', 'close')
                self.end_headers()
                self.wfile.write(data)
                self.close_connection = True
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    origin = 'http://127.0.0.1:' + str(server.server_port)
    cases = []
    try:
        for kind in ['gzip', 'chunked', 'close', 'raw', 'badgzip', '404', '302', 'retry', 'repo', 'force', 'badforce', '304', 'unsigneddenied', '304empty', 'proxy', 'permanent503', 'shortretry', 'shortalways', 'crc', 'concat', 'truncated', 'empty', 'extra', 'ascii', 'path-space']:
            case = out / kind
            case.mkdir()
            directory = case / 'extensions'
            start = len(records)
            config = vp()
            db = vp()
            con = vp()
            err = vp()
            assert lib.duckdb_create_config(ctypes.byref(config)) == 0
            for key, value in [(b'allow_unsigned_extensions', b'false' if kind == 'unsigneddenied' else b'true'), (b'extension_directory', str(directory).encode())]:
                assert lib.duckdb_set_config(config, key, value) == 0
            if kind == 'proxy':
                for key, value in [(b'http_proxy', ('127.0.0.1:' + str(server.server_port)).encode()), (b'http_proxy_username', b'fixture-user'), (b'http_proxy_password', b'fixture-pass')]:
                    assert lib.duckdb_set_config(config, key, value) == 0
            assert lib.duckdb_open_ext(None, ctypes.byref(db), config, ctypes.byref(err)) == 0
            lib.duckdb_destroy_config(ctypes.byref(config))
            assert lib.duckdb_connect(db, ctypes.byref(con)) == 0
            url = origin + '/' + kind + ('/space name' if kind == 'path-space' else '') + '/c_only_probe.duckdb_extension.gz'
            repo = origin + '/' + kind + '/repo'
            if kind == 'proxy':
                url = 'http://example.invalid/proxy/c_only_probe.duckdb_extension.gz'
            sql = "INSTALL '" + url + "'" if kind != 'repo' else "INSTALL c_only_probe FROM '" + repo + "'"
            ok = kind not in ['raw', 'badgzip', '404', '302', 'unsigneddenied', '304empty', 'permanent503', 'shortalways', 'truncated', 'empty', 'extra', 'ascii']
            first = query(kind + '-install', sql, ok)
            if not ok:
                assert first['status'] != 0
            path = directory / 'v1.5.6' / platform / 'c_only_probe.duckdb_extension'
            info = Path(str(path) + '.info')
            before = None
            if ok:
                assert path.read_bytes() == raw['v1']
                assert b'"fixture-v1"' in info.read_bytes()
                before = (sha(path), sha(info))
                (case / 'initial.info').write_bytes(info.read_bytes())
                n = len(requests)
                query(kind + '-repeat', sql)
                assert len(requests) == n
            if kind in ['force', 'badforce', '304']:
                phase[kind] = 'force'
                result = query(kind + '-force', 'FORCE ' + sql, kind != 'badforce')
                if kind == 'force':
                    assert path.read_bytes() == raw['v2']
                elif kind == 'badforce':
                    assert result['status'] != 0 and before == (sha(path), sha(info))
                else:
                    assert before == (sha(path), sha(info))
            if ok:
                query(kind + '-catalog', "SELECT extension_name,loaded,installed,extension_version,install_mode,installed_from FROM duckdb_extensions() WHERE extension_name='c_only_probe'")
                query(kind + '-load', 'LOAD c_only_probe')
                assert query(kind + '-function', 'SELECT c_only_add7(5)')['rows'] == [['12']]
            else:
                assert not path.exists() and (not info.exists())
            assert not list(directory.rglob('*.tmp-*'))
            assert query(kind + '-database-valid', 'SELECT 42')['rows'] == [['42']]
            lib.duckdb_disconnect(ctypes.byref(con))
            lib.duckdb_close(ctypes.byref(db))
            cases.append({'kind': kind, 'records': records[start:], 'requests': [r for r in requests if r['kind'] == kind]})
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    (out / 'verification.json').write_text(json.dumps({'cases': cases, 'origin': origin, 'requests': requests, 'scope': 'PublicCAPI localHTTP transport/install regression; caller authenticates library and C-only fixtures before/after. Not finalreleaseacceptance.', 'utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}, indent=2) + '\n')
    print('TWENTY_FIVE_HTTP_CASES_PASS', flush=True)
if __name__ == '__main__':
    main()
