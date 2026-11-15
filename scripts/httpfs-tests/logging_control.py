#!/usr/bin/env python3
"""Observe real mixed-case headers, redirects and the deprecated logging callback."""
import argparse
import hashlib
import http.server
import json
from pathlib import Path
import threading

from s3_version_control import API, quote


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['library', 'package', 'parquet', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    payload = args.parquet.read_bytes()
    requests = []
    lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *args):
            pass

        def do_HEAD(self):
            self.respond()

        def do_GET(self):
            self.respond()

        def respond(self):
            with lock:
                requests.append(dict(method=self.command, path=self.path, range=self.headers.get('Range')))
            if self.path == '/redirect.parquet':
                self.send_response(302)
                self.send_header('Location', '/file.parquet')
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            start, end = 0, len(payload) - 1
            status = 200
            if self.command == 'GET' and self.headers.get('Range'):
                start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                status = 206
            self.send_response(status)
            self.send_header('cOnTeNt-LeNgTh', str(end - start + 1))
            if status == 206:
                self.send_header('CoNtEnT-RaNgE', f'bytes {start}-{end}/{len(payload)}')
            self.send_header('x-FiXtUrE', 'first')
            self.send_header('x-fixture', 'second')
            self.send_header('VaRy', 'Accept')
            self.send_header('vary', 'Origin')
            self.send_header('ETag', '"owned"')
            self.end_headers()
            if self.command == 'GET':
                self.wfile.write(payload[start:end + 1])

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    records = []
    try:
        for backend in ['curl', 'httplib']:
            directory = out / backend
            directory.mkdir()
            api = API(args.library, directory / 'secrets')
            try:
                def run(sql):
                    result = api.query(sql)
                    records.append(dict(backend=backend, **result))
                    (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                    assert result['status'] == 0, result

                run(f"LOAD '{quote(args.package.resolve())}'")
                run(f"SET httpfs_client_implementation='{backend}'; SET enable_external_file_cache=false; CALL enable_logging('HTTP')")
                for path in ['file.parquet', 'redirect.parquet']:
                    run('CALL truncate_duckdb_logs()')
                    run(f"SELECT count(*) FROM read_parquet('http://127.0.0.1:{server.server_port}/{path}')")
                    run("SELECT request.type, response.headers['x-FiXtUrE'], response.headers['VaRy'], response.headers['CoNtEnT-RaNgE'], response.headers['Location'] FROM duckdb_logs_parsed('HTTP') ORDER BY timestamp")
                run('CALL truncate_duckdb_logs(); SET enable_http_logging=false')
                run(f"SELECT count(*) FROM read_parquet('http://127.0.0.1:{server.server_port}/file.parquet')")
                run('SELECT count(*) FROM duckdb_logs')
                # Re-enabling the manager does not reset the client's HTTP flag.
                run("CALL enable_logging('HTTP')")
                run(f"SELECT count(*) FROM read_parquet('http://127.0.0.1:{server.server_port}/file.parquet')")
                run('SELECT count(*) FROM duckdb_logs')
                run('SET enable_http_logging=true')
                run("SELECT current_setting('enable_logging'), current_setting('logging_level'), current_setting('enabled_log_types'), current_setting('logging_storage')")
                run("SET logging_storage='memory'")
                run(f"SELECT count(*) FROM read_parquet('http://127.0.0.1:{server.server_port}/file.parquet')")
                run("SELECT count(*) FROM duckdb_logs WHERE type='HTTP'")
                run('SET enable_http_logging=false; RESET enable_http_logging')
                run("SELECT current_setting('enable_http_logging'), current_setting('enable_logging')")
            finally:
                api.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    (out / 'verification.json').write_text(json.dumps(dict(records=records, requests=requests,
        sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                for name in ['library', 'package', 'parquet']}, stopped=True), indent=2) + '\n')
    print(f'{len(records)} logging records captured', flush=True)


if __name__ == '__main__':
    main()
