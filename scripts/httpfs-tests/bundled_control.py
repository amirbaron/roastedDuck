#!/usr/bin/env python3
"""Exercise bundled loading and real HTTP reads; optionally compare the C++ oracle."""
import argparse
import hashlib
import http.server
import json
from pathlib import Path
import subprocess
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', type=Path, required=True)
    parser.add_argument('--oracle', type=Path)
    parser.add_argument('--oracle-httpfs', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if bool(args.oracle) != bool(args.oracle_httpfs):
        parser.error('--oracle and --oracle-httpfs must be provided together')
    args.out.mkdir(parents=True, exist_ok=False)
    records = []
    requests = []
    body = b'value\n1\n2\n3\n'
    payloads = {'/data.csv': body}
    if args.oracle:
        parquet = args.out.resolve() / 'oracle.parquet'
        sql = "COPY (SELECT * FROM (VALUES (1),(2),(3)) t(value)) TO '" + str(parquet).replace("'", "''") + "' (FORMAT parquet);"
        subprocess.run([str(args.oracle.resolve()), '-batch', '-bail', '-init', '/dev/null',
                        ':memory:'], input=sql, text=True, capture_output=True, check=True, timeout=30)
        payloads['/data.parquet'] = parquet.read_bytes()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_HEAD(self):
            self.respond(False)

        def do_GET(self):
            self.respond(True)

        def respond(self, send_body):
            requests.append({'method': self.command, 'path': self.path,
                             'range': self.headers.get('Range')})
            body = payloads[self.path]
            start, end = 0, len(body) - 1
            if self.headers.get('Range'):
                value = self.headers['Range'].removeprefix('bytes=')
                first, last = value.split('-', 1)
                start = int(first)
                end = min(int(last) if last else end, end)
                self.send_response(206)
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(body)}')
            else:
                self.send_response(200)
            data = body[start:end + 1]
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Content-Type', 'text/csv')
            self.send_header('ETag', '"bundled-fixture"')
            self.end_headers()
            if send_body:
                self.wfile.write(data)

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    url = f'http://127.0.0.1:{server.server_port}/data.csv'
    query = f"SELECT sum(value) FROM read_csv('{url}', header=true, columns={{'value':'INTEGER'}});"

    def run(engine, cli, case, sql, expected=None, error=None, no_requests=False):
        request_count = len(requests)
        result = subprocess.run([str(cli.resolve()), '-batch', '-bail', '-csv', '-noheader',
                                 '-init', '/dev/null', ':memory:'], input=sql,
                                text=True, capture_output=True, timeout=45)
        passed = ((result.returncode == 0 and result.stdout.strip() == expected)
                  if error is None else
                  (result.returncode != 0 and error.lower() in
                   (result.stdout + result.stderr).lower()))
        if no_requests:
            passed = passed and len(requests) == request_count
        records.append(dict(engine=engine, case=case, sql=sql, exit=result.returncode,
                            stdout=result.stdout, stderr=result.stderr, passed=passed))
        # Retain failure evidence before raising.
        (args.out / 'results.json').write_text(json.dumps(records, indent=2) + '\n')
        assert passed, f'{engine}/{case}: {result.stdout} {result.stderr}'

    try:
        offline = "SET extension_directory='/dev/null/bundled-httpfs'; SET custom_extension_repository='http://127.0.0.1:1';\n"
        run('rust', args.cli, 'offline-install-read', offline +
            'INSTALL httpfs; FORCE INSTALL s3; LOAD httpfs; LOAD https;\n' + query, '6')
        run('rust', args.cli, 'autoload-read', offline +
            'SET autoload_known_extensions=true;\n' + query, '6')
        run('rust', args.cli, 'autoload-disabled', offline +
            'SET autoload_known_extensions=false;\n' + query,
            error='requires the extension httpfs', no_requests=True)
        run('rust', args.cli, 'autoload-disabled-filesystem', offline +
            "SET autoload_known_extensions=true; SET disabled_filesystems='HTTPFileSystem';\n" + query,
            error='disabled', no_requests=True)
        run('rust', args.cli, 'load-does-not-reset-settings', offline +
            "LOAD httpfs; SET s3_region='keep-me'; LOAD s3; SELECT current_setting('s3_region');", 'keep-me')
        run('rust', args.cli, 'external-access-disabled', offline +
            'LOAD httpfs; SET enable_external_access=false;\n' + query,
            error='disabled by configuration', no_requests=True)
        run('rust', args.cli, 'reject-bundled-version', offline +
            "INSTALL httpfs VERSION 'unrelated';", error='bundled')
        if args.oracle:
            path = str(args.oracle_httpfs.resolve()).replace("'", "''")
            run('cpp', args.oracle, 'http-read', f"LOAD '{path}';\n" + query, '6')
            parquet_query = f"SELECT sum(value) FROM read_parquet('{url.replace('data.csv', 'data.parquet')}');"
            run('rust', args.cli, 'autoload-parquet', offline +
                'SET autoload_known_extensions=true;\n' + parquet_query, '6')
            run('cpp', args.oracle, 'http-parquet', f"LOAD '{path}';\n" + parquet_query, '6')
        assert any(r['method'] == 'GET' for r in requests)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
        (args.out / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')
    identity = {'cli_sha256': hashlib.sha256(args.cli.read_bytes()).hexdigest(),
                'cases': len(records), 'passed': all(r['passed'] for r in records),
                'scope': 'Bundled loading and loopback HTTP; not S3/Azure/cloud acceptance'}
    if args.oracle:
        identity['oracle_sha256'] = hashlib.sha256(args.oracle.read_bytes()).hexdigest()
        identity['oracle_httpfs_sha256'] = hashlib.sha256(args.oracle_httpfs.read_bytes()).hexdigest()
    (args.out / 'verification.json').write_text(json.dumps(identity, indent=2) + '\n')
    print(json.dumps(identity))


if __name__ == '__main__':
    main()
