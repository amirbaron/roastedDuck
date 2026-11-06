"""Observe secret transactions via two public C API connections.

Caller authenticates the native library before/after. Only isolated test secret
directories and synthetic header values are used. Records capture outcomes from
the engine rather than assuming transaction semantics in the observer.
"""
import argparse
import ctypes
import importlib.util
import json
import http.server
import threading
from pathlib import Path


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('library', type=Path)
    p.add_argument('out', type=Path)
    p.add_argument('--conflicts', action='store_true')
    p.add_argument('--package', type=Path)
    args = p.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('s3_version_control', Path(__file__).with_name('s3_version_control.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    records, files, requests = [], [], []
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass
        def respond(self):
            version = self.headers.get('X-Version', '')
            value = {'original': 1, 'pending': 2, 'first': 3, 'second': 4}.get(version, 0)
            data = f'i\n{value}\n'.encode()
            requests.append({'path': self.path, 'method': self.command, 'version': version})
            status, headers = 200, {}
            if self.command == 'GET' and self.headers.get('Range'):
                start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                headers['Content-Range'] = f'bytes {start}-{end}/{len(data)}'
                data = data[start:end + 1]
                status = 206
            self.send_response(status)
            self.send_header('Content-Length', str(len(data)))
            self.send_header('ETag', f'"fixture-{value}"')
            for name, value in headers.items():
                self.send_header(name, value)
            self.end_headers()
            if self.command == 'GET':
                self.wfile.write(data)
        do_HEAD = do_GET = respond
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    def http_sql(case, label):
        return f"SELECT sum(i) FROM read_csv('http://127.0.0.1:{server.server_port}/{case}/{label}.csv')"

    inspect = "SELECT name,persistent,storage,secret_string FROM duckdb_secrets() WHERE name='tx_secret' ORDER BY name"
    for persistent in [False, True]:
        for op in ['create', 'replace', 'drop']:
            for ending in ['COMMIT', 'ROLLBACK']:
                for snapshot in [False, True]:
                    kind = 'PERSISTENT' if persistent else 'TEMPORARY'
                    case = '-'.join([kind.lower(), op, ending.lower(), 'snapshot' if snapshot else 'current'])
                    directory = args.out / case / 'secrets'
                    api = module.API(args.library, directory)
                    a = api.con
                    b = ctypes.c_void_p()
                    assert api.lib.duckdb_connect(api.db, ctypes.byref(b)) == 0
                    def query(connection, label, sql):
                        api.con = a if connection == 'A' else b
                        rec = {'case': case, 'connection': connection, 'label': label, **api.query(sql)}
                        records.append(rec)
                        (args.out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                        return rec
                    def observe_files(label):
                        files.append({'case': case, 'label': label, 'files': sorted(file.name for file in directory.glob('*.duckdb_secret'))})
                    try:
                        if args.package:
                            assert query('A', 'load', f"LOAD '{module.quote(args.package / 'httpfs.duckdb_extension')}'")['status'] == 0
                            assert query('A', 'no-proxy', "SET http_proxy=''")['status'] == 0
                        if op != 'create':
                            rec = query('A', 'seed', f"CREATE {kind} SECRET tx_secret (TYPE http, EXTRA_HTTP_HEADERS MAP {{'X-Version':'original'}})")
                            assert rec['status'] == 0, rec
                        if snapshot:
                            assert query('B', 'reader-begin', 'BEGIN')['status'] == 0
                            assert query('B', 'reader-anchor', inspect)['status'] == 0
                        assert query('A', 'writer-begin', 'BEGIN')['status'] == 0
                        sql = (f'DROP {kind} SECRET tx_secret' if op == 'drop' else
                               f"CREATE {'OR REPLACE ' if op == 'replace' else ''}{kind} SECRET tx_secret (TYPE http, EXTRA_HTTP_HEADERS MAP {{'X-Version':'pending'}})")
                        query('A', 'write', sql)
                        query('A', 'writer-during', inspect)
                        query('B', 'reader-during', inspect)
                        if args.package:
                            query('A', 'writer-http-during', http_sql(case, 'writer-during'))
                            query('B', 'reader-http-during', http_sql(case, 'reader-during'))
                        observe_files('during')
                        query('A', 'writer-end', ending)
                        query('A', 'writer-after', inspect)
                        query('B', 'reader-after', inspect)
                        if args.package:
                            query('A', 'writer-http-after', http_sql(case, 'writer-after'))
                            query('B', 'reader-http-after', http_sql(case, 'reader-after'))
                        observe_files('after')
                        if snapshot:
                            query('B', 'reader-end', 'COMMIT')
                            query('B', 'reader-refreshed', inspect)
                            if args.package:
                                query('B', 'reader-http-refreshed', http_sql(case, 'reader-refreshed'))
                    finally:
                        api.lib.duckdb_disconnect(ctypes.byref(b))
                        api.con = a
                        api.close()
                    # A new database instance sees the actual persistent files.
                    fresh = module.API(args.library, directory)
                    try:
                        records.append({'case': case, 'connection': 'fresh', 'label': 'reopen', **fresh.query(inspect)})
                    finally:
                        fresh.close()
    if args.conflicts:
        for persistent in [False, True]:
            kind = 'PERSISTENT' if persistent else 'TEMPORARY'
            scenarios = [(op, op, late) for op in ['create', 'replace', 'drop'] for late in [False, True]]
            scenarios += [('create', 'drop', False), ('create', 'different', False)]
            for first, second, late in scenarios:
                case = '-'.join([kind.lower(), 'conflict', first, second, 'committed' if late else 'active'])
                directory = args.out / case / 'secrets'
                api = module.API(args.library, directory)
                a, b = api.con, ctypes.c_void_p()
                assert api.lib.duckdb_connect(api.db, ctypes.byref(b)) == 0
                def query(connection, label, sql):
                    api.con = a if connection == 'A' else b
                    rec = {'case': case, 'connection': connection, 'label': label, **api.query(sql)}
                    records.append(rec)
                    (args.out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                    return rec
                def change(operation, version, name='tx_secret'):
                    return (f'DROP {kind} SECRET {name}' if operation == 'drop' else
                            f"CREATE {'OR REPLACE ' if operation == 'replace' else ''}{kind} SECRET {name} (TYPE http, EXTRA_HTTP_HEADERS MAP {{'X-Version':'{version}'}})")
                try:
                    if args.package:
                        assert query('A', 'load', f"LOAD '{module.quote(args.package / 'httpfs.duckdb_extension')}'")['status'] == 0
                        assert query('A', 'no-proxy', "SET http_proxy=''")['status'] == 0
                    if first != 'create':
                        assert query('A', 'seed', change('create', 'original'))['status'] == 0
                    assert query('A', 'first-begin', 'BEGIN')['status'] == 0
                    assert query('B', 'second-begin', 'BEGIN')['status'] == 0
                    assert query('B', 'second-anchor', inspect)['status'] == 0
                    assert query('A', 'first-write', change(first, 'first'))['status'] == 0
                    if late:
                        query('A', 'first-end', 'COMMIT')
                    query('B', 'second-write', change('create' if second == 'different' else second, 'second', 'other_secret' if second == 'different' else 'tx_secret'))
                    query('B', 'second-after', inspect)
                    if args.package:
                        query('B', 'second-http', http_sql(case, 'second-after'))
                    if not late:
                        query('A', 'first-end', 'COMMIT')
                    query('B', 'second-end', 'COMMIT' if second == 'different' else 'ROLLBACK')
                    query('A', 'final', 'SELECT name,persistent,storage,secret_string FROM duckdb_secrets() ORDER BY name')
                    if args.package:
                        query('A', 'final-http', http_sql(case, 'final'))
                    files.append({'case': case, 'label': 'after', 'files': sorted(file.name for file in directory.glob('*.duckdb_secret'))})
                finally:
                    api.lib.duckdb_disconnect(ctypes.byref(b))
                    api.con = a
                    api.close()
                fresh = module.API(args.library, directory)
                try:
                    records.append({'case': case, 'connection': 'fresh', 'label': 'reopen', **fresh.query('SELECT name,persistent,storage,secret_string FROM duckdb_secrets() ORDER BY name')})
                finally:
                    fresh.close()
    server.shutdown()
    server.server_close()
    (args.out / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')
    (args.out / 'records.json').write_text(json.dumps(records, indent=2) + '\n')
    (args.out / 'files.json').write_text(json.dumps(files, indent=2) + '\n')
    print(json.dumps({'cases': len(set(r['case'] for r in records)), 'records': len(records)}))


if __name__ == '__main__':
    main()
