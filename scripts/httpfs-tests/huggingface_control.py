"""HuggingFace C API controls through an isolated TLS CONNECT receiver.

Caller authenticates library/package. The receiver never opens an upstream socket:
only huggingface.co and cdn.fixture.invalid CONNECT targets are accepted. Synthetic
certificates and tokens; independent C++ Parquet input. No real HF service proof.
"""
import argparse
import ctypes
import http.server
import json
import os
from pathlib import Path
import re
import socketserver
import ssl
import threading
import urllib.parse


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('library', type=Path)
    p.add_argument('package', type=Path)
    p.add_argument('tls', type=Path)
    p.add_argument('parquet', type=Path)
    p.add_argument('out', type=Path)
    p.add_argument('--tls-matrix', type=Path)
    args = p.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
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
        f = getattr(lib, name)
        f.argtypes, f.restype = params, result
    lib.duckdb_destroy_result.argtypes = [ctypes.POINTER(Result)]
    lib.duckdb_free.argtypes = [vp]
    for name in ['duckdb_destroy_config', 'duckdb_disconnect', 'duckdb_close']:
        getattr(lib, name).argtypes = [pp]
    records, requests, connects, cases = [], [], [], []
    active = {}
    data = {'data.csv': b'i,s\n1,one\n2,two\n', 'a.csv': b'i,s\n3,three\n',
            'n.csv': b'i,s\n4,four\n', 'd.csv': b'i,s\n5,five\n', 'data.parquet': args.parquet.read_bytes()}
    tree = {'': [('file', 'data.csv'), ('file', 'a.csv'), ('directory', 'nested')],
            '/nested': [('file', 'nested/n.csv'), ('directory', 'nested/deep')],
            '/nested/deep': [('file', 'nested/deep/d.csv')]}
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.tls / 'server-cert.pem', args.tls / 'server-key.pem')

    class HTTP(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *unused):
            pass

        def do_GET(self):
            self.respond(True)

        def do_HEAD(self):
            self.respond(False)

        def respond(self, body):
            parsed = urllib.parse.urlsplit(self.path)
            host = self.headers.get('Host')
            auth = self.headers.get('Authorization')
            expected = None if host == 'cdn.fixture.invalid' else active['authorization']
            valid = auth == expected
            record = {'case': active['case'], 'method': self.command, 'host': host,
                      'path': self.path, 'authorization_valid': valid, 'authorization_present': auth is not None,
                      'range': self.headers.get('Range')}
            requests.append(record)
            payload, status, link, location = None, 200, None, None
            if '/tree/' in parsed.path:
                directory = parsed.path.split('/tree/', 1)[1].split('/', 1)
                directory = '/' + directory[1] if len(directory) == 2 else ''
                entries = [{'type': kind, 'path': path, 'size': len(data.get(path.rsplit('/', 1)[-1], b'')),
                            'lastCommit': {'id': 'fixture'}} for kind, path in tree.get(directory, [])]
                params = urllib.parse.parse_qs(parsed.query)
                limit = int(params.get('limit', ['1000'])[0])
                offset = int(params.get('cursor', ['0'])[0])
                payload = json.dumps(entries[offset:offset + limit], separators=(',', ':'), ensure_ascii=False).encode()
                if offset + limit < len(entries):
                    link = '<https://huggingface.co' + parsed.path + '?cursor=' + str(offset + limit) + '&limit=' + str(limit) + '>; rel="next"'
                if active['case'] == 'bad-link':
                    link = '<https://huggingface.co/api/ignored>; rel="prev"'
                if active['case'] == 'malformed-list':
                    payload = b'[{"type":"file","size":42}]'
                if active['case'] == 'list-error':
                    status, payload = 403, b'fixture denied'
            else:
                name = parsed.path.rsplit('/', 1)[-1]
                payload = data.get(name)
                if name == 'redirect.csv':
                    status, payload = 302, b''
                    location = '/datasets/org/repo/resolve/main/data.csv'
                if name == 'cdn.csv':
                    status, payload = 302, b''
                    location = 'https://cdn.fixture.invalid/data.csv'
            if not valid:
                status, payload = 401, b'fixture unauthorized'
            elif payload is None:
                status, payload = 404, b'fixture missing'
            start, end = 0, len(payload) - 1
            if body and status == 200 and self.headers.get('Range'):
                match = re.fullmatch(r'bytes=(\d+)-(\d+)', self.headers['Range'])
                assert match
                start, end = int(match[1]), min(int(match[2]), end)
                status = 206
            record['status'] = status
            self.send_response(status)
            self.send_header('Content-Length', str(end - start + 1))
            self.send_header('ETag', '"fixture-hf-v1"')
            self.send_header('Last-Modified', 'Wed, 01 Jan 2020 00:00:00 GMT')
            if link:
                self.send_header('Link', link)
            if location:
                self.send_header('Location', location)
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(payload)}')
            self.end_headers()
            if body:
                self.wfile.write(payload[start:end + 1])

    class Proxy(socketserver.StreamRequestHandler):
        def handle(self):
            line = self.rfile.readline().decode().strip()
            assert line in ['CONNECT huggingface.co:443 HTTP/1.1', 'CONNECT cdn.fixture.invalid:443 HTTP/1.1'], line
            while self.rfile.readline().strip():
                pass
            connects.append({'case': active['case'], 'target': line.split()[1]})
            self.wfile.write(b'HTTP/1.1 200 Connection Established\r\n\r\n')
            self.wfile.flush()
            with active.get('server_tls', context).wrap_socket(self.connection, server_side=True) as connection:
                HTTP(connection, self.client_address, self.server)

    class Server(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        daemon_threads = True

    server = Server(('127.0.0.1', 0), Proxy)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    proxy = 'http://127.0.0.1:' + str(server.server_address[1])
    quote = lambda value: str(value).replace("'", "''")

    def query(label, sql, expected=None, success=True):
        r = Result()
        code = lib.duckdb_query(con, sql.encode(), ctypes.byref(r))
        error = lib.duckdb_result_error(ctypes.byref(r))
        rows = []
        if code == 0:
            for row in range(lib.duckdb_row_count(ctypes.byref(r))):
                values = []
                for column in range(lib.duckdb_column_count(ctypes.byref(r))):
                    if lib.duckdb_value_is_null(ctypes.byref(r), column, row):
                        values.append(None)
                    else:
                        value = lib.duckdb_value_varchar(ctypes.byref(r), column, row)
                        values.append(ctypes.string_at(value).decode())
                        lib.duckdb_free(value)
                rows.append(values)
        record = {'case': active['case'], 'label': label, 'sql': sql, 'status': code,
                  'error': error.decode() if error else None, 'rows': rows}
        records.append(record)
        lib.duckdb_destroy_result(ctypes.byref(r))
        (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        assert (code == 0) == success, record
        if expected is not None:
            assert rows == expected, record

    keys = ['HF_TOKEN', 'HF_TOKEN_PATH', 'HF_HOME', 'NO_PROXY', 'no_proxy']
    saved = {key: os.environ.get(key) for key in keys}
    original_working_directory = Path.cwd()
    kinds = ['public', 'token', 'scoped', 'http-fallback', 'token-overrides-http', 'extra-overrides-token',
                     'empty-token', 'chain-env', 'chain-path', 'chain-home', 'chain-home-empty', 'chain-path-missing', 'chain-home-missing',
                     'spaces-revision', 'glob', 'pagination', 'bad-link', 'list-error', 'malformed-list', 'invalid-url',
                     'redirect', 'cdn-redirect', 'missing-file']
    if args.tls_matrix:
        kinds += ['tls-chain', 'tls-untrusted', 'tls-wrong-host', 'tls-expired', 'tls-httplib-wrong-host',
                  'tls-httplib-expired', 'tls-httplib-default-wrong-host', 'tls-disabled-expired']
    try:
        for kind in kinds:
            for key in keys:
                os.environ.pop(key, None)
            os.chdir(original_working_directory)
            os.environ['NO_PROXY'] = os.environ['no_proxy'] = ''
            active.clear()
            active.update(case=kind, authorization=None)
            if kind.startswith('tls-'):
                leaf = 'wrong' if 'wrong-host' in kind else 'expired' if 'expired' in kind else 'good'
                server_tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                server_tls.load_cert_chain(args.tls_matrix / (leaf + '-cert.pem'), args.tls_matrix / (('good' if leaf == 'expired' else leaf) + '-key.pem'))
                active['server_tls'] = server_tls
            case = out / kind
            case.mkdir()
            (case / 'token').write_bytes(b'fixture-path\r\nignored-second-line\n')
            home = case / 'hf-home'
            home.mkdir()
            (home / 'token').write_bytes(b'fixture-home\nignored\n')
            if kind == 'chain-env':
                os.environ.update(HF_TOKEN='fixture-env', HF_TOKEN_PATH=str(case / 'token'), HF_HOME=str(home))
                active['authorization'] = 'Bearer fixture-env'
            elif kind == 'chain-path':
                os.environ.update(HF_TOKEN_PATH=str(case / 'token'), HF_HOME=str(home))
                active['authorization'] = 'Bearer fixture-path'
            elif kind == 'chain-home':
                os.environ['HF_HOME'] = str(home)
                active['authorization'] = 'Bearer fixture-home'
            elif kind == 'chain-home-empty':
                os.environ['HF_HOME'] = ''
                os.chdir(home)
                active['authorization'] = 'Bearer fixture-home'
            elif kind == 'chain-path-missing':
                os.environ.update(HF_TOKEN_PATH=str(case / 'missing-token'), HF_HOME=str(home))
            elif kind == 'chain-home-missing':
                os.environ['HF_HOME'] = str(case / 'missing-home')
            db, con, config, error = vp(), vp(), vp(), vp()
            start = len(records)
            assert lib.duckdb_create_config(ctypes.byref(config)) == 0
            for key, value in [('allow_unsigned_extensions', 'false'), ('allow_community_extensions', 'false'),
                               ('autoinstall_known_extensions', 'false'), ('autoload_known_extensions', 'false'),
                               ('secret_directory', str(case / 'secrets'))]:
                assert lib.duckdb_set_config(config, key.encode(), value.encode()) == 0
            assert lib.duckdb_open_ext(None, ctypes.byref(db), config, ctypes.byref(error)) == 0
            lib.duckdb_destroy_config(ctypes.byref(config))
            assert lib.duckdb_connect(db, ctypes.byref(con)) == 0
            try:
                query('load', f"LOAD '{quote(args.package / 'httpfs.duckdb_extension')}'")
                query('proxy', f"SET http_proxy='{proxy}'")
                ca = '' if kind == 'tls-untrusted' else args.tls_matrix / 'root-cert.pem' if kind.startswith('tls-') else args.tls / 'server-cert.pem'
                query('ca', f"SET ca_cert_file='{quote(ca)}'")
                query('retry', 'SET http_retries=0')
                query('type', "SELECT type,default_provider,extension FROM duckdb_secret_types() WHERE type='huggingface'", [['huggingface', 'config', 'httpfs']])
                if kind.startswith('tls-httplib'):
                    query('httplib', "SET httpfs_client_implementation='httplib'")
                    if 'default' not in kind:
                        query('httplib-strict', 'SET enable_server_cert_verification=true')
                if kind == 'tls-disabled-expired':
                    query('disable-verification', 'SET enable_curl_server_cert_verification=false')
                if kind in ['http-fallback', 'token-overrides-http', 'extra-overrides-token', 'empty-token']:
                    token = 'fixture-token' if kind == 'http-fallback' else 'wrong-http-token'
                    extra = ",EXTRA_HTTP_HEADERS MAP {'Authorization':'Bearer fixture-header'}" if kind == 'extra-overrides-token' else ''
                    query('http-secret', f"CREATE SECRET http_creds (TYPE http, SCOPE 'hf://', BEARER_TOKEN '{token}'{extra})")
                    if kind == 'http-fallback':
                        active['authorization'] = 'Bearer fixture-token'
                if kind in ['token', 'scoped', 'token-overrides-http', 'extra-overrides-token', 'empty-token', 'redirect', 'cdn-redirect']:
                    token = '' if kind == 'empty-token' else 'fixture-token'
                    if kind == 'scoped':
                        query('wrong-scope-default', "CREATE SECRET wrong (TYPE huggingface,TOKEN 'wrong-token')")
                    scope = ",SCOPE 'hf://datasets/org/repo/'" if kind == 'scoped' else ''
                    query('hf-secret', f"CREATE SECRET hf (TYPE huggingface,TOKEN '{token}'{scope})")
                    active['authorization'] = 'Bearer fixture-header' if kind == 'extra-overrides-token' else 'Bearer fixture-token' if token else None
                if kind.startswith('chain-'):
                    query('chain', "CREATE SECRET hf (TYPE huggingface,PROVIDER credential_chain)", success=not kind.endswith('missing'))
                query('redacted-secrets', 'SELECT type,provider,scope::VARCHAR,secret_string FROM duckdb_secrets() ORDER BY name')
                if kind.startswith('tls-'):
                    accepted = kind in ['tls-chain', 'tls-httplib-default-wrong-host', 'tls-disabled-expired']
                    query('tls-csv', "SELECT sum(i),count(*) FROM read_csv('hf://datasets/org/repo/data.csv')", [['3', '2']] if accepted else None, success=accepted)
                elif kind == 'invalid-url':
                    for uri in ['hf://datasets/org', 'hf://datasets/org/repo', 'hf://models/org/repo/data.csv']:
                        query('invalid', f"SELECT * FROM glob('{uri}')", success=False)
                elif kind in ['glob', 'pagination', 'bad-link', 'list-error', 'malformed-list']:
                    if kind == 'pagination':
                        query('page-limit', 'SET hf_max_per_page=2')
                    query('glob', "SELECT file FROM glob('hf://datasets/org/repo@main/*.csv') ORDER BY file",
                          [['hf://datasets/org/repo/a.csv'], ['hf://datasets/org/repo/data.csv']] if kind in ['glob', 'pagination'] else None,
                          success=kind in ['glob', 'pagination'])
                    if kind in ['glob', 'pagination']:
                        query('recursive', "SELECT file FROM glob('hf://datasets/org/repo/**/*.csv') ORDER BY file",
                              [[uri] for uri in ['hf://datasets/org/repo/a.csv', 'hf://datasets/org/repo/data.csv', 'hf://datasets/org/repo/nested/deep/d.csv', 'hf://datasets/org/repo/nested/n.csv']])
                else:
                    uri = 'hf://spaces/org/space@v1' if kind == 'spaces-revision' else 'hf://datasets/org/repo'
                    name = 'redirect.csv' if kind == 'redirect' else 'cdn.csv' if kind == 'cdn-redirect' else 'missing.csv' if kind == 'missing-file' else 'data.csv'
                    query('csv', f"SELECT sum(i),count(*) FROM read_csv('{uri}/{name}')", [['3', '2']] if kind != 'missing-file' else None, success=kind != 'missing-file')
                    if kind != 'missing-file':
                        query('parquet', f"SELECT sum(i),count(*) FROM read_parquet('{uri}/data.parquet')", [['4950', '100']])
                query('database-valid', 'SELECT 42', [['42']])
            finally:
                lib.duckdb_disconnect(ctypes.byref(con))
                lib.duckdb_close(ctypes.byref(db))
            cases.append({'kind': kind, 'records': len(records) - start})
        assert requests and connects
        assert all(r['authorization_valid'] for r in requests), [r for r in requests if not r['authorization_valid']]
        assert any(r['host'] == 'cdn.fixture.invalid' and not r['authorization_present'] for r in requests)
        (out / 'verification.json').write_text(json.dumps({'records': records, 'cases': cases, 'requests': requests, 'connects': connects,
            'scope': 'Synthetic TLS proxy HF file/tree/token controls, no upstream sockets or default user-token reads. Not real HF/full HTTPFS/release acceptance.'}, indent=2) + '\n')
        print('HF_CONTROLS_PASS', len(cases), len(records), flush=True)
    finally:
        os.chdir(original_working_directory)
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        server.shutdown()
        server.server_close()
        thread.join()
        (out / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')
        (out / 'connects.json').write_text(json.dumps(connects, indent=2) + '\n')


if __name__ == '__main__':
    main()
