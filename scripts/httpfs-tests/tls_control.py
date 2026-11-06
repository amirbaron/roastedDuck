"""Native HTTP(S) provider regression using a loopback TLS server.

Caller must authenticate the SDK binary and native fixtures before and after.
TLS files are a synthetic server-cert.pem/server-key.pem pair. Parquet data must
come from an independent C++ reference. The SDK control asserts query answers.
This tests explicit embedding registration, not SQL LOAD httpfs availability.
"""
import argparse
import subprocess
import sys
import json
import ssl
import http.server
import threading
import urllib.parse
import re
import gzip
from pathlib import Path

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('sdk', type=Path)
    parser.add_argument('fixtures', type=Path)
    parser.add_argument('tls', type=Path)
    parser.add_argument('parquet', type=Path, help='C++-generated data.parquet,a.parquet,b.parquet directory')
    parser.add_argument('out', type=Path)
    a = parser.parse_args()
    sdk = a.sdk.resolve()
    probe = a.fixtures.resolve()
    tls = a.tls.resolve()
    out = a.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    requests = []
    changed = set()
    parquet_a = a.parquet / 'a.parquet'
    parquet_b = a.parquet / 'b.parquet'
    data = {'data.csv': b'i,s\n1,one\n2,two\n', 'data.parquet': (a.parquet / 'data.parquet').read_bytes(), 'c_only_probe.duckdb_extension.gz': gzip.compress((probe / 'raw-v1/c_only_probe.duckdb_extension').read_bytes(), mtime=0)}

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
            requests.append({'kind': kind, 'method': self.command, 'path': self.path, 'range': self.headers.get('Range')})
            payload = data.get(name)
            if kind == 'etag' and kind in changed:
                payload = b'i,s\n3,one\n4,two\n'
            if kind == 'etag-parquet':
                payload = (parquet_b if kind in changed else parquet_a).read_bytes()
            if kind == 'switch':
                payload = b'ok'
            status = 200
            start = 0
            end = len(payload) - 1 if payload is not None else 0
            if name == 'redirect.csv':
                self.send_response(302)
                self.send_header('Location', '/' + kind + '/data.csv')
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            if payload is None:
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
        for kind in ['trusted', 'untrusted', 'disabled-verification', 'httplib-default', 'httplib-strict', 'redirect', 'parquet', 'install', 'missing', 'etag', 'etag-parquet']:
            with (out / (kind + '.log')).open('x') as log:
                child = subprocess.run(['/usr/bin/nice', '-n', '10', str(sdk), origin, str(tls / 'server-cert.pem'), kind, str(probe), str(out / 'cases' / kind)], stdout=log, stderr=subprocess.STDOUT)
            record = {'kind': kind, 'exit_code': child.returncode, 'output': (out / (kind + '.log')).read_text(), 'requests': [r for r in requests if r['kind'] == kind]}
            cases.append(record)
            (out / 'progress.json').write_text(json.dumps(cases, indent=2) + '\n')
            print(kind, child.returncode, record['output'][-1000:], flush=True)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    (out / 'verification.json').write_text(json.dumps({'cases': cases, 'origin': origin, 'scope': 'NativeSDK explicit HTTP(S) provider loopback TLS controls; caller authenticates native inputs. Not SQL LOAD httpfs, all functionality, or release acceptance.'}, indent=2) + '\n')
    assert all((c['exit_code'] == 0 for c in cases))
if __name__ == '__main__':
    main()
