#!/usr/bin/env python3
"""Differential download/threshold/range controls with independent HTTP receipts."""
import argparse
import hashlib
import http.server
import json
from pathlib import Path
import threading
import urllib.parse

from s3_version_control import API, quote


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['library', 'package', 'parquet', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    payload = args.parquet.read_bytes()
    receipts = []
    lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *unused):
            pass

        def handle(self):
            try:
                super().handle()
            except ConnectionResetError:
                self.close_connection = True

        def do_HEAD(self):
            self.respond()

        def do_GET(self):
            self.respond()

        def respond(self):
            path = urllib.parse.urlsplit(self.path).path
            body = b'' if path == '/empty' else payload
            length = len(body) + (1 if path == '/wrong-head.parquet' and self.command == 'HEAD' else 0)
            status = 403 if path == '/head-denied.parquet' and self.command == 'HEAD' else 200
            requested = self.headers.get('Range')
            start, end = 0, len(body) - 1
            if requested and path == '/range.parquet':
                start, end = map(int, requested.removeprefix('bytes=').split('-'))
                end = min(end, len(body) - 1)
                body = body[start:end + 1]
                length, status = len(body), 206
            with lock:
                receipts.append(dict(method=self.command, path=path, range=requested,
                                     status=status, user_agent=self.headers.get('User-Agent')))
            self.send_response(status)
            self.send_header('Content-Length', str(length if status != 403 else 0))
            self.send_header('ETag', '"fixture-version"')
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(payload)}')
            self.end_headers()
            if self.command == 'GET' and status != 403:
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    self.close_connection = True

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    api = API(args.library, out / 'secrets')
    base = f'http://127.0.0.1:{server.server_port}'
    records = []
    try:
        for sql in [f"LOAD '{quote(args.package.resolve())}'",
                    "SET enable_external_file_cache=false; SET enable_http_metadata_cache=false; CALL enable_logging()"]:
            result = api.query(sql)
            assert result['status'] == 0, result
        scenarios = [
            ('range', '/range.parquet', False, 0, True, False),
            ('forced', '/range.parquet', True, 0, True, False),
            ('forced-new-query', '/range.parquet', True, 0, True, False),
            ('threshold-above', '/range.parquet', False, len(payload) + 1, True, False),
            ('threshold-equal', '/range.parquet', False, len(payload), True, False),
            ('fallback', '/no-range.parquet', False, 0, True, False),
            ('fallback-disabled', '/no-range.parquet', False, 0, False, True),
            ('wrong-head', '/wrong-head.parquet', False, 0, True, True),
            ('forced-head-denied', '/head-denied.parquet', True, 0, True, False),
            ('forced-empty', '/empty', True, 0, True, False),
        ]
        for label, path, forced, threshold, fallback, error in scenarios:
            config = api.query(f"SET force_download={str(forced).lower()}; SET force_download_threshold={threshold}; SET auto_fallback_to_full_download={str(fallback).lower()}; CALL truncate_duckdb_logs()")
            assert config['status'] == 0, config
            with lock:
                receipts.clear()
            sql = f"SELECT count(*) FROM read_parquet('{base}{path}')" if path != '/empty' else f"SELECT octet_length(content) FROM read_blob('{base}{path}')"
            result = api.query(sql)
            assert bool(result['status']) == error, result
            warnings = api.query("SELECT message FROM duckdb_logs WHERE log_level='WARNING'")
            assert warnings['status'] == 0, warnings
            with lock:
                requests = list(receipts)
            assert requests and all((r['user_agent'] or '').startswith('duckdb/') for r in requests)
            records.append(dict(label=label, result=result, warnings=warnings['rows'], requests=requests))
            (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
    finally:
        api.close()
        server.shutdown()
        server.server_close()
        thread.join()
    (out / 'verification.json').write_text(json.dumps(dict(records=records, stopped=True,
        sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                for name in ['library', 'package', 'parquet']}), indent=2) + '\n')
    print(f'{len(records)} download scenarios passed', flush=True)


if __name__ == '__main__':
    main()
