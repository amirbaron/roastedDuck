"""Observe secret cleanup through public C API disconnect/error/interrupt endings.

Caller authenticates the native engine before/after. Only private test stores are
used. Results are observations for paired C++/Rust review, not assumed verdicts.
"""
import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('library', type=Path)
parser.add_argument('out', type=Path)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
root = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('control', root / 'scripts/httpfs-tests/s3_version_control.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
records, files = [], []
inspect = 'SELECT name,persistent,storage,secret_string FROM duckdb_secrets() ORDER BY name'
for persistent in [False, True]:
    for operation in ['create', 'replace', 'drop']:
        for ending in ['disconnect', 'interrupt', 'failed-commit']:
            for snapshot in [False, True]:
                kind = 'PERSISTENT' if persistent else 'TEMPORARY'
                case = f'{kind.lower()}-{operation}-{ending}-snapshot{int(snapshot)}'
                directory = args.out / case / 'secrets'
                api = module.API(args.library, directory)
                a, b = api.con, ctypes.c_void_p()
                assert api.lib.duckdb_connect(api.db, ctypes.byref(b)) == 0
                api.lib.duckdb_interrupt.argtypes = [ctypes.c_void_p]
                api.lib.duckdb_interrupt.restype = None
                disconnected = False
                def query(connection, label, sql):
                    api.con = a if connection == 'A' else b
                    result = {'case': case, 'connection': connection, 'label': label, **api.query(sql)}
                    records.append(result)
                    (args.out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                    return result
                try:
                    assert query('A', 'threads', 'SET threads=1')['status'] == 0
                    assert query('A', 'failure-table', 'CREATE TABLE failure_target(i INTEGER)')['status'] == 0
                    if operation != 'create':
                        assert query('A', 'seed', f"CREATE {kind} SECRET item(TYPE http,EXTRA_HTTP_HEADERS MAP {{'X-Version':'original'}})")['status'] == 0
                    if snapshot:
                        assert query('B', 'reader-begin', 'BEGIN')['status'] == 0
                        query('B', 'reader-anchor', inspect)
                    assert query('A', 'writer-begin', 'BEGIN')['status'] == 0
                    sql = f'DROP {kind} SECRET item' if operation == 'drop' else f"CREATE {'OR REPLACE ' if operation == 'replace' else ''}{kind} SECRET item(TYPE http,EXTRA_HTTP_HEADERS MAP {{'X-Version':'pending'}})"
                    assert query('A', 'write', sql)['status'] == 0
                    query('A', 'writer-during', inspect)
                    query('B', 'reader-during', inspect)
                    files.append({'case': case, 'label': 'during', 'files': sorted(f.name for f in directory.glob('*.duckdb_secret'))})
                    if ending == 'disconnect':
                        api.lib.duckdb_disconnect(ctypes.byref(a))
                        disconnected = True
                    elif ending == 'failed-commit':
                        result = query('A', 'execution-error', "INSERT INTO failure_target SELECT s::INTEGER FROM (VALUES ('not-an-integer'))t(s)")
                        assert result['status'] == 1, result
                        query('A', 'commit-after-error', 'COMMIT')
                        query('A', 'writer-after', inspect)
                    else:
                        api.con = a
                        outcome = []
                        thread = threading.Thread(target=lambda: outcome.append(api.query('SELECT sum(hash(i)) FROM range(100000000000)t(i)')), daemon=True)
                        thread.start()
                        deadline = time.monotonic() + 5
                        while thread.is_alive() and time.monotonic() < deadline:
                            time.sleep(0.02)
                            api.lib.duckdb_interrupt(a)
                            thread.join(0.01)
                        if thread.is_alive():
                            (args.out / 'interrupt-timeout.json').write_text(json.dumps({'case': case}) + '\n')
                            os._exit(124)
                        assert len(outcome) == 1 and outcome[0]['status'] == 1, outcome
                        # Timing-dependent progress/error decoration stays raw; the
                        # semantic interrupt observation records status and class.
                        records.append({'case': case, 'connection': 'A', 'label': 'interrupted',
                                        **outcome[0]})
                        query('A', 'commit-after-interrupt', 'COMMIT')
                        query('A', 'writer-after', inspect)
                    query('B', 'reader-after', inspect)
                    if snapshot:
                        query('B', 'reader-end', 'COMMIT')
                        query('B', 'reader-refreshed', inspect)
                    files.append({'case': case, 'label': 'after', 'files': sorted(f.name for f in directory.glob('*.duckdb_secret'))})
                finally:
                    if disconnected:
                        api.con = b
                    else:
                        api.lib.duckdb_disconnect(ctypes.byref(b))
                        api.con = a
                    api.close()
                fresh = module.API(args.library, directory)
                try:
                    records.append({'case': case, 'connection': 'fresh', 'label': 'reopen', **fresh.query(inspect)})
                finally:
                    fresh.close()
                (args.out / 'records.json').write_text(json.dumps(records, indent=2) + '\n')
                (args.out / 'files.json').write_text(json.dumps(files, indent=2) + '\n')
print('SECRET_LIFECYCLE_OBSERVED', len({r['case'] for r in records}), len(records))
