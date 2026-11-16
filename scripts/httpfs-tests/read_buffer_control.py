#!/usr/bin/env python3
"""Bounded owned data for blob-cache and compressed-JSON request-count checks."""
import argparse
import gzip
import hashlib
import http.server
import json
from pathlib import Path
import threading
import urllib.parse

from s3_version_control import API, quote


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['library', 'extensions', 'parquet', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    data = out / 'data'
    data.mkdir()
    blob = bytes(range(256)) * 12288
    (data / 'blob.bin').write_bytes(blob)
    with (data / 'items.json.gz').open('wb') as raw:
        with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0, compresslevel=1) as stream:
            for index in range(32768):
                value = hashlib.shake_256(str(index).encode()).hexdigest(256)
                stream.write(json.dumps(dict(id=index, payload=value), separators=(',', ':')).encode() + b'\n')
    # The same rows uncompressed and as CSV, a text file, and a pinned multi-megabyte
    # Parquet file: every reader that opens a remote file without direct I/O.
    with gzip.open(data / 'items.json.gz', 'rb') as stream:
        lines = stream.read()
    (data / 'items.json').write_bytes(lines)
    rows = b'id,payload\n' + b''.join(
        f"{item['id']},{item['payload']}\n".encode() for item in map(json.loads, lines.splitlines()))
    (data / 'items.csv').write_bytes(rows)
    with (data / 'items.csv.gz').open('wb') as raw:
        with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0, compresslevel=1) as stream:
            stream.write(rows)
    (data / 'notes.txt').write_bytes(b'owned text fixture\n' * 100000)
    (data / 'meta.bin').write_bytes(blob[:4096])
    (data / 'table.parquet').write_bytes(args.parquet.read_bytes())
    hashes = {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in data.iterdir()}
    requests = []
    lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *unused):
            pass

        def do_HEAD(self):
            self.respond()

        def do_GET(self):
            self.respond()

        def respond(self):
            name = urllib.parse.urlsplit(self.path).path.removeprefix('/')
            if name not in hashes:
                self.send_error(404)
                return
            path = data / name
            size = path.stat().st_size
            start, end, status = 0, size - 1, 200
            requested = self.headers.get('Range')
            if self.command == 'GET' and requested:
                start, end = map(int, requested.removeprefix('bytes=').split('-'))
                end = min(end, size - 1)
                status = 206
            with lock:
                requests.append(dict(method=self.command, file=name, range=requested, status=status,
                                     bytes=end - start + 1 if self.command == 'GET' else 0))
            self.send_response(status)
            self.send_header('Content-Length', str(end - start + 1))
            self.send_header('ETag', '"' + hashes[name] + '"')
            if status == 206:
                self.send_header('Content-Range', f'bytes {start}-{end}/{size}')
            self.end_headers()
            if self.command == 'GET':
                with path.open('rb') as source:
                    source.seek(start)
                    remaining = end - start + 1
                    try:
                        while remaining:
                            block = source.read(min(remaining, 65536))
                            self.wfile.write(block)
                            remaining -= len(block)
                    except (BrokenPipeError, ConnectionResetError):
                        self.close_connection = True

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    api = API(args.library, out / 'secrets')
    base = f'http://127.0.0.1:{server.server_port}'
    records = []
    try:
        result = api.query(f"LOAD '{quote((args.extensions / 'httpfs.duckdb_extension').resolve())}'")
        assert result['status'] == 0, result
        # JSON is part of roastedDuck; the reference library loads its own package.
        if api.query('LOAD json')['status']:
            result = api.query(f"LOAD '{quote((args.extensions / 'json.duckdb_extension').resolve())}'")
            assert result['status'] == 0, result
        blob_sql = f"SELECT octet_length(content),sha256(content) FROM read_blob('{base}/blob.bin')"
        cases = [
            ('blob-first', blob_sql, [[str(len(blob)), hashes['blob.bin']]]),
            ('blob-repeat', blob_sql, [[str(len(blob)), hashes['blob.bin']]]),
            ('blob-filename', f"SELECT filename FROM read_blob('{base}/blob.bin')", [[base + '/blob.bin']]),
            ('json-first', f"SELECT count(*),sum(id),min(length(payload)),max(length(payload)) FROM read_json_auto('{base}/items.json.gz')", [['32768', '536854528', '512', '512']]),
            ('json-repeat', f"SELECT count(*),sum(id) FROM read_json_auto('{base}/items.json.gz')", [['32768', '536854528']]),
            # Compared between engines rather than against fixed rows.
            ('json-plain-first', f"SELECT count(*),sum(id) FROM read_json_auto('{base}/items.json')", None),
            ('json-plain-repeat', f"SELECT count(*),sum(id) FROM read_json_auto('{base}/items.json')", None),
            ('csv-plain', f"SELECT count(*),sum(id) FROM read_csv('{base}/items.csv')", None),
            ('csv-gzip', f"SELECT count(*),sum(id) FROM read_csv('{base}/items.csv.gz')", None),
            ('text-all', f"SELECT filename,length(content),size FROM read_text('{base}/notes.txt')", None),
            ('blob-metadata', f"SELECT size,last_modified IS NOT NULL FROM read_blob('{base}/meta.bin')", None),
            ('blob-all', f"SELECT filename,octet_length(content),size FROM read_blob('{base}/meta.bin')", None),
            ('parquet-count', f"SELECT count(*) FROM read_parquet('{base}/table.parquet')", None),
            ('parquet-scan', f"SELECT count(*),bit_xor(hash(t)) FROM read_parquet('{base}/table.parquet') t", None),
            ('cache-ranges', f"SELECT replace(path,'{base}',''),location,nr_bytes FROM duckdb_external_file_cache() ORDER BY ALL", None),
        ]
        for name, sql, expected in cases:
            with lock:
                before = len(requests)
            result = api.query(sql)
            with lock:
                sent = list(requests[before:])
            record = dict(name=name, result=result, requests=sent,
                          get_count=sum(r['method'] == 'GET' for r in sent))
            records.append(record)
            (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
            assert result['status'] == 0 and (expected is None or result['rows'] == expected), record
        passed = records[0]['get_count'] == 1 and records[1]['get_count'] == 0 and records[3]['get_count'] < 20
    finally:
        api.close()
        server.shutdown()
        server.server_close()
        thread.join()
    (out / 'verification.json').write_text(json.dumps(dict(records=records, passed=passed,
        data_sha256=hashes, library_sha256=hashlib.sha256(args.library.read_bytes()).hexdigest(),
        stopped=True), indent=2) + '\n')
    print([(r['name'], r['get_count']) for r in records], flush=True)
    if not passed:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
