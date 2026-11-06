"""Compare HTTP profiles with an independent loopback receiver's byte receipts.

The caller authenticates the library/package before and after execution. Payloads
are deterministic and small; neither Content-Length nor cached data count as I/O.
"""
import argparse
import ctypes
import gzip
import hashlib
import http.server
import json
import re
import threading
from pathlib import Path

from s3_version_control import API, quote


def profile(text):
    lines = [line.strip('│ ') for line in text.splitlines()]
    if 'HTTPFS HTTP Stats' not in lines:
        return None
    return dict(line.split(': ', 1) for line in lines if re.match(r'^(in|out|#HEAD|#GET|#PUT|#POST|#DELETE): ', line))


def formatted(size):
    parts = [size]
    for _ in range(5):
        parts.append(parts[-1] // 1024)
        parts[-2] %= 1024
    units = ['bytes', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB']
    for i in range(5, 0, -1):
        if parts[i]:
            return f'{parts[i]}.{parts[i-1] * 10 // 1024} {units[i]}'
    return f'{size} ' + ('byte' if size == 1 else 'bytes')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    parser.add_argument('package', type=Path)
    parser.add_argument('out', type=Path)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    payloads = {
        '/range.blob': b'profile\x00payload\xff\n',
        '/chunked.blob': b'chunked\x00payload\xff\n',
        '/retry.blob': b'retry payload\n',
        '/rows.json': b'[{"x":3},{"x":7}]',
        '/rows.csv.gz': gzip.compress(b'x\n' + b'3\n7\n' * 10000, mtime=0),
    }
    receipts, records = [], []
    lock = threading.Lock()
    retry = 0

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *unused):
            pass

        def do_HEAD(self):
            self.respond()

        def do_GET(self):
            self.respond()

        def respond(self):
            nonlocal retry
            data = payloads[self.path]
            body = data
            status = 200
            start, end = 0, len(data) - 1
            requested = self.headers.get('Range')
            if self.command == 'GET' and requested and self.path != '/chunked.blob':
                start, end = (int(x) for x in requested.removeprefix('bytes=').split('-'))
                end = min(end, len(data) - 1)
                body = data[start:end + 1]
                status = 206
            with lock:
                if self.path == '/retry.blob' and self.command == 'GET':
                    retry += 1
                    if retry <= 2:
                        body, status = b'busy', 503
            chunked = self.command == 'GET' and self.path == '/chunked.blob'
            with lock:
                receipts.append({'method': self.command, 'path': self.path, 'status': status,
                                 'body_bytes': len(body) if self.command != 'HEAD' else 0,
                                 'range': requested, 'sha256': hashlib.sha256(body).hexdigest()})
            self.send_response(status)
            self.send_header('ETag', '"profile-v1"')
            if chunked:
                self.send_header('Transfer-Encoding', 'chunked')
            else:
                self.send_header('Content-Length', str(len(body)))
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(data)}')
            self.end_headers()
            if self.command != 'HEAD':
                if chunked:
                    for offset in range(0, len(body), 3):
                        part = body[offset:offset+3]
                        self.wfile.write(f'{len(part):x}\r\n'.encode() + part + b'\r\n')
                    self.wfile.write(b'0\r\n\r\n')
                else:
                    self.wfile.write(body)

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    api = API(args.library, out / 'secrets')
    origin = f'http://127.0.0.1:{server.server_port}'

    def query(sql):
        result = api.query(sql)
        assert result['status'] == 0, result
        return result['rows']

    def check(label, sql, ordinary=False, expected=None):
        begin = len(receipts)
        if ordinary:
            target = out / (label + '.txt')
            query(f"SET profiling_output='{quote(target)}'")
            query("PRAGMA enable_profiling='query_tree'")
            rows = query(sql)
            query('PRAGMA disable_profiling')
            text = target.read_text()
            assert rows == expected, (label, rows, expected)
        else:
            rows = query('EXPLAIN ANALYZE ' + sql)
            text = rows[0][1]
            (out / (label + '.txt')).write_text(text)
        actual = profile(text)
        wire = receipts[begin:]
        expected_profile = {f'#{method}': str(sum(r['method'] == method for r in wire))
                            for method in ['HEAD', 'GET', 'PUT', 'POST', 'DELETE']}
        expected_profile.update({'in': formatted(sum(r['body_bytes'] for r in wire)), 'out': '0 bytes'})
        record = {'label': label, 'profile': actual, 'wire': wire, 'expected_profile': expected_profile}
        records.append(record)
        (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        present = bool(wire) or label in ['after-http', 'ordinary-local']
        assert (actual is not None) == present, record
        if present:
            assert actual == expected_profile, record

    try:
        check('before-httpfs', 'SELECT 42')
        query(f"LOAD '{quote(args.package.resolve())}'")
        query('SET enable_external_file_cache=false')
        query('SET http_retries=2')
        query('SET http_retry_wait_ms=1')
        check('loaded-no-io', 'SELECT 42')
        for name in ['range.blob', 'chunked.blob', 'retry.blob']:
            check(name, f"SELECT octet_length(content) FROM read_blob('{origin}/{name}')")
        check('json', f"SELECT sum(x) FROM read_json('{origin}/rows.json')")
        check('gzip', f"SELECT sum(x) FROM read_csv('{origin}/rows.csv.gz')")
        check('after-http', 'SELECT 42')
        check('ordinary-http', f"SELECT octet_length(content) FROM read_blob('{origin}/range.blob')",
              ordinary=True, expected=[[str(len(payloads['/range.blob']))]])
        check('ordinary-local', 'SELECT 42', ordinary=True, expected=[['42']])
        original = api.con
        second = ctypes.c_void_p()
        assert api.lib.duckdb_connect(api.db, ctypes.byref(second)) == 0
        try:
            api.con = second
            check('second-connection-no-io', 'SELECT 42')
        finally:
            api.con = original
            api.lib.duckdb_disconnect(ctypes.byref(second))
        (out / 'verification.json').write_text(json.dumps({'records': records, 'receipts': receipts,
            'scope': 'Loopback response-body bytes, retries, compressed reads, query-tree/EXPLAIN profiles and connection/reset isolation; not performance acceptance.'}, indent=2) + '\n')
        print('HTTP_PROFILES_PASS', len(records), flush=True)
    finally:
        api.close()
        server.shutdown()
        server.server_close()
        worker.join()
        (out / 'requests.json').write_text(json.dumps(receipts, indent=2) + '\n')


if __name__ == '__main__':
    main()
