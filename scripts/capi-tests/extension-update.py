"""Exercise direct and prepared UPDATE EXTENSIONS, repository ETags, result tags and failure preservation.

The caller authenticates the library and raw-v1/raw-v2 C-only fixtures before
and after execution. Generated inputs contain synthetic or authenticated bytes.
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
        if prepared_mode and sql.startswith('UPDATE EXTENSIONS'):
            statement = vp()
            status = lib.duckdb_prepare(con, sql.encode(), ctypes.byref(statement))
            assert status == 0, lib.duckdb_prepare_error(statement)
            code = lib.duckdb_execute_prepared(statement, ctypes.byref(r))
            lib.duckdb_destroy_prepare(ctypes.byref(statement))
        else:
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
        lib.duckdb_column_name.argtypes = [ctypes.POINTER(Result), ctypes.c_uint64]
        lib.duckdb_column_name.restype = ctypes.c_char_p
        lib.duckdb_column_type.argtypes = [ctypes.POINTER(Result), ctypes.c_uint64]
        lib.duckdb_column_type.restype = ctypes.c_uint
        lib.duckdb_result_statement_type.argtypes = [Result]
        lib.duckdb_result_statement_type.restype = ctypes.c_uint
        columns = [{'name': lib.duckdb_column_name(ctypes.byref(r), i).decode(), 'type': lib.duckdb_column_type(ctypes.byref(r), i)} for i in range(lib.duckdb_column_count(ctypes.byref(r)))] if code == 0 else []
        record = {'columns': columns, 'statement_type': lib.duckdb_result_statement_type(r), 'label': label, 'sql': sql, 'status': code, 'error': error.decode() if error else None, 'rows': data}
        records.append(record)
        lib.duckdb_destroy_result(ctypes.byref(r))
        (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        if expect_success:
            assert code == 0, record
        return record
    quote = lambda path: str(path).replace("'", "''")
    import http.server, threading, gzip
    prepared_mode = False
    for name, args, restype in [('duckdb_prepare', [vp, ctypes.c_char_p, pp], ctypes.c_int), ('duckdb_prepare_error', [vp], ctypes.c_char_p), ('duckdb_execute_prepared', [vp, ctypes.POINTER(Result)], ctypes.c_int), ('duckdb_destroy_prepare', [pp], None)]:
        f = getattr(lib, name)
        f.argtypes = args
        f.restype = restype
    phase = {}
    requests = []
    raw = {v: (fixture / ('raw-' + v) / 'c_only_probe.duckdb_extension').read_bytes() for v in ['v1', 'v2']}

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *args):
            pass

        def do_GET(self):
            kind = self.path.split('/')[1]
            state = phase.get(kind, 'initial')
            etag = '"v2"' if state == 'new' and kind in ['new', 'sameversion', 'noversion', 'prepared'] else '"v1"'
            status = 200
            body = raw['v2' if kind in ['new', 'prepared'] and state == 'new' else 'v1']
            tag = self.headers.get('If-None-Match')
            requests.append({'kind': kind, 'path': self.path, 'if_none_match': tag})
            if kind == 'noversion' and state == 'new':
                body = body[:-384] + bytes(32) + body[-352:]
            if state == 'new' and kind == 'fail':
                status = 503
                body = b'busy'
            elif state == 'new' and tag == etag:
                status = 304
                body = b''
            elif status == 200:
                body = gzip.compress(body, mtime=0)
            self.send_response(status)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('ETag', etag)
            self.end_headers()
            self.wfile.write(body)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    origin = 'http://127.0.0.1:' + str(server.server_port)
    cases = []
    try:
        for kind in ['empty', 'missing', 'builtin', 'custom', 'missinginfo', 'unknowninfo', 'corruptinfo', 'badmetadata', 'etag', 'new', 'sameversion', 'noversion', 'fail', 'prepared']:
            case = out / kind
            case.mkdir()
            directory = case / 'extensions'
            start = len(records)
            config = vp()
            db = vp()
            con = vp()
            err = vp()
            assert lib.duckdb_create_config(ctypes.byref(config)) == 0
            for key, value in [(b'allow_unsigned_extensions', b'true'), (b'extension_directory', str(directory).encode())]:
                assert lib.duckdb_set_config(config, key, value) == 0
            assert lib.duckdb_open_ext(None, ctypes.byref(db), config, ctypes.byref(err)) == 0
            lib.duckdb_destroy_config(ctypes.byref(config))
            assert lib.duckdb_connect(db, ctypes.byref(con)) == 0
            path = directory / 'v1.5.6' / platform / 'c_only_probe.duckdb_extension'
            info = Path(str(path) + '.info')
            repo = origin + '/' + kind + '/repo'
            if kind not in ['empty', 'missing', 'builtin']:
                if kind in ['custom', 'missinginfo', 'unknowninfo', 'corruptinfo', 'badmetadata']:
                    query(kind + '-install', "INSTALL '" + quote(fixture / 'raw-v1/c_only_probe.duckdb_extension') + "'")
                else:
                    query(kind + '-install', "INSTALL c_only_probe FROM '" + repo + "'")
                if kind == 'missinginfo':
                    info.unlink()
                if kind == 'unknowninfo':
                    b = info.read_bytes()
                    assert b[:3] == bytes([100, 0, 2])
                    info.write_bytes(b[:2] + bytes(1) + b[3:])
                if kind == 'corruptinfo':
                    info.write_bytes(b'not valid metadata')
                if kind == 'badmetadata':
                    b = path.read_bytes()
                    assert b[-288:-287] == b'4'
                    path.write_bytes(b[:-288] + b'X' + b[-287:])
            before = (sha(path), sha(info)) if path.exists() and info.exists() else None
            phase[kind] = 'new'
            prepared_mode = kind == 'prepared'
            sql = 'UPDATE EXTENSIONS' if kind == 'empty' else 'UPDATE EXTENSIONS (' + ('json' if kind == 'builtin' else 'c_only_probe') + ')'
            result = query(kind + '-update', sql, kind not in ['missing', 'builtin', 'corruptinfo', 'badmetadata', 'fail'])
            prepared_mode = False
            if kind in ['missing', 'builtin', 'corruptinfo', 'badmetadata', 'fail']:
                assert result['status'] != 0
            if kind in ['etag', 'fail']:
                assert before == (sha(path), sha(info))
            if kind in ['new', 'prepared']:
                assert path.read_bytes() == raw['v2']
            if kind == 'new':
                query(kind + '-second-update', sql)
            if info.exists():
                (case / 'final.info').write_bytes(info.read_bytes())
            assert not list(directory.rglob('*.tmp-*'))
            assert query(kind + '-database-valid', 'SELECT 42')['rows'] == [['42']]
            lib.duckdb_disconnect(ctypes.byref(con))
            lib.duckdb_close(ctypes.byref(db))
            cases.append({'kind': kind, 'records': records[start:], 'requests': [r for r in requests if r['kind'] == kind]})
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    (out / 'verification.json').write_text(json.dumps({'cases': cases, 'origin': origin, 'scope': 'Public C API UPDATE EXTENSIONS direct/prepared, missing/custom/metadata/repo/ETag304/newversion/sameversion/emptyversion/failurepreservation; nativecodefixturesauthenticated, UPDATE itselfdoesnotloadnativecode.', 'utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}, indent=2) + '\n')
    print(json.dumps([(c['kind'], next((x for x in c['records'] if x['label'].endswith('-update')))) for c in cases], indent=2), flush=True)
if __name__ == '__main__':
    main()
