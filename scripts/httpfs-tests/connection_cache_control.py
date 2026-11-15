#!/usr/bin/env python3
"""Compare cache events and independently observed TCP reuse on an owned server."""
import argparse
import ctypes
import hashlib
import http.server
import json
import os
from pathlib import Path
import threading

from s3_version_control import API, quote


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['library', 'package', 'parquet', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    for name in list(os.environ):
        if name.lower() in ['http_proxy', 'https_proxy', 'all_proxy', 'no_proxy']:
            os.environ.pop(name)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    payload = args.parquet.read_bytes()
    requests = []
    lock = threading.Lock()

    class Server(http.server.ThreadingHTTPServer):
        connections = 0
        identities = {}

        def get_request(self):
            sock, address = super().get_request()
            with lock:
                self.connections += 1
                self.identities[id(sock)] = self.connections
            return sock, address

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *args):
            pass

        def do_HEAD(self):
            self.respond()

        def do_GET(self):
            self.respond()

        def respond(self):
            start, end = 0, len(payload) - 1
            status = 200
            if self.command == 'GET' and self.headers.get('Range'):
                start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                status = 206
            with lock:
                requests.append(dict(connection=self.server.identities[id(self.connection)],
                                     method=self.command, path=self.path, range=self.headers.get('Range'),
                                     authorization=self.headers.get('Authorization')))
            self.send_response(status)
            self.send_header('Content-Length', str(end - start + 1))
            self.send_header('ETag', '"owned"')
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(payload)}')
            self.end_headers()
            if self.command == 'GET':
                self.wfile.write(payload[start:end + 1])

    server = Server(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    api = API(args.library, out / 'secrets')
    first = api.con
    second = ctypes.c_void_p()
    records = []
    try:
        def run(sql):
            result = api.query(sql)
            assert result['status'] == 0, result
            return result

        run(f"LOAD '{quote(args.package.resolve())}'")
        assert api.lib.duckdb_connect(api.db, ctypes.byref(second)) == 0
        run("SET enable_external_file_cache=false; CALL enable_logging('HTTPFSInfo')")
        cases = [
            ('default-off', '', 'a.parquet'),
            ('enabled-cold', 'SET httpfs_connection_caching=true', 'a.parquet'),
            ('enabled-warm', '', 'a.parquet'),
            ('shared-connection', '', 'a.parquet'),
            ('same-origin', '', 'b.parquet'),
            ('auth-first', "CREATE SECRET cache_auth(TYPE HTTP, EXTRA_HTTP_HEADERS MAP {'Authorization':'Bearer owned-first'})", 'a.parquet'),
            ('auth-replaced', "CREATE OR REPLACE SECRET cache_auth(TYPE HTTP, EXTRA_HTTP_HEADERS MAP {'Authorization':'Bearer owned-second'})", 'a.parquet'),
            ('auth-dropped', 'DROP SECRET cache_auth', 'a.parquet'),
            ('timeout-change', 'SET http_timeout=60', 'a.parquet'),
            ('disabled', 'SET httpfs_connection_caching=false', 'a.parquet'),
            ('reenabled', 'SET httpfs_connection_caching=true', 'b.parquet'),
            ('backend-reset', "SET httpfs_client_implementation='curl'", 'a.parquet'),
            ('reset-enabled', 'SET httpfs_connection_caching=true', 'a.parquet'),
            ('httplib', "SET httpfs_client_implementation='httplib'; SET httpfs_connection_caching=true", 'a.parquet'),
            ('httplib-again', '', 'a.parquet'),
            ('back-to-curl', "SET httpfs_client_implementation='curl'", 'a.parquet'),
            ('curl-enabled', 'SET httpfs_connection_caching=true', 'a.parquet'),
            ('proxy-cold', f"SET httpfs_client_implementation='curl'; SET http_proxy='http://127.0.0.1:{server.server_port}'; SET httpfs_connection_caching=true", 'a.parquet'),
            ('proxy-warm', '', 'b.parquet'),
        ]
        for label, settings, path in cases:
            api.con = second if label == 'shared-connection' else first
            if settings:
                run(settings)
            run('CALL truncate_duckdb_logs()')
            with lock:
                before = len(requests)
            result = run(f"SELECT count(*) FROM read_parquet('http://127.0.0.1:{server.server_port}/{path}')")
            events = run("SELECT message FROM duckdb_logs WHERE type='HTTPFSInfo' ORDER BY timestamp")
            with lock:
                sent = list(requests[before:])
            setting = run("SELECT current_setting('httpfs_connection_caching')")['rows']
            records.append(dict(label=label, result=result, events=events['rows'], requests=sent, setting=setting))
            (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        by_name = {r['label']: r for r in records}
        def connection(name):
            ids = {r['connection'] for r in by_name[name]['requests']}
            assert len(ids) == 1, by_name[name]
            return next(iter(ids))
        assert connection('enabled-cold') == connection('enabled-warm') == connection('same-origin') == connection('reenabled')
        assert connection('timeout-change') == connection('enabled-cold')
        assert connection('shared-connection') == connection('enabled-cold')
        for name, authorization in [('auth-first', 'Bearer owned-first'), ('auth-replaced', 'Bearer owned-second'), ('auth-dropped', None)]:
            assert connection(name) == connection('enabled-cold')
            assert all(r['authorization'] == authorization for r in by_name[name]['requests'])
        assert connection('disabled') != connection('enabled-cold')
        assert connection('reset-enabled') != connection('enabled-cold')
        assert connection('httplib') != connection('httplib-again')
        assert connection('proxy-cold') == connection('proxy-warm')
        assert all(r['path'].startswith('http://') for name in ['proxy-cold', 'proxy-warm'] for r in by_name[name]['requests'])
    finally:
        api.con = first
        if second.value:
            api.lib.duckdb_disconnect(ctypes.byref(second))
        api.close()
        server.shutdown()
        server.server_close()
        thread.join()
    (out / 'verification.json').write_text(json.dumps(dict(records=records, accepted_connections=server.connections,
        sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                for name in ['library', 'package', 'parquet']}, stopped=True), indent=2) + '\n')
    print(f'{len(records)} connection scenarios passed', flush=True)


if __name__ == '__main__':
    main()
