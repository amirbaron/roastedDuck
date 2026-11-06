"""Compare secret rollback after a real commit I/O failure, using private stores.

A directory at the not-yet-created WAL filename causes COMMIT to fail. Every path
belongs to this test output. Caller authenticates the library before and after.
"""
import argparse
import ctypes
import importlib.util
import json
from pathlib import Path

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
        for snapshot in [False, True]:
            kind = 'PERSISTENT' if persistent else 'TEMPORARY'
            case = f'{kind.lower()}-{operation}-snapshot{int(snapshot)}'
            directory = args.out / case
            directory.mkdir()
            store = directory / 'secrets'
            api = module.API(args.library, store)
            a, b = api.con, ctypes.c_void_p()
            assert api.lib.duckdb_connect(api.db, ctypes.byref(b)) == 0
            def query(connection, label, sql):
                api.con = a if connection == 'A' else b
                r = {'case': case, 'connection': connection, 'label': label, **api.query(sql)}
                records.append(r)
                (args.out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                return r
            db = directory / 'fixture.db'
            try:
                for label, sql in [('attach', f"ATTACH '{module.quote(db)}' AS d"),
                                   ('table', 'CREATE TABLE d.t(i INTEGER)'), ('checkpoint', 'CHECKPOINT d')]:
                    assert query('A', label, sql)['status'] == 0
                if operation != 'create':
                    assert query('A','seed',f"CREATE {kind} SECRET item(TYPE http,EXTRA_HTTP_HEADERS MAP {{'X-Version':'original'}})")['status'] == 0
                if snapshot:
                    assert query('B','reader-begin','BEGIN')['status'] == 0
                    query('B','reader-anchor',inspect)
                assert query('A','writer-begin','BEGIN')['status'] == 0
                sql = f'DROP {kind} SECRET item' if operation == 'drop' else f"CREATE {'OR REPLACE ' if operation == 'replace' else ''}{kind} SECRET item(TYPE http,EXTRA_HTTP_HEADERS MAP {{'X-Version':'pending'}})"
                assert query('A','write',sql)['status'] == 0
                assert query('A','insert','INSERT INTO d.t VALUES(1)')['status'] == 0
                query('A','writer-during',inspect)
                query('B','reader-during',inspect)
                wal = Path(str(db) + '.wal')
                assert not wal.exists(), wal
                wal.mkdir()
                result = query('A','commit-io-failure','COMMIT')
                assert result['status'] == 1 and 'Failed to commit' in result['error'], result
                assert query('A','table-after','SELECT * FROM d.t')['rows'] == []
                query('A','writer-after',inspect)
                query('B','reader-after',inspect)
                if snapshot:
                    query('B','reader-end','COMMIT')
                    query('B','reader-refreshed',inspect)
                files.append({'case':case,'files':sorted(f.name for f in store.glob('*.duckdb_secret'))})
            finally:
                api.lib.duckdb_disconnect(ctypes.byref(b))
                api.con = a
                api.close()
            fresh = module.API(args.library, store)
            try:
                records.append({'case':case,'connection':'fresh','label':'reopen',**fresh.query(inspect)})
            finally:
                fresh.close()
(args.out/'records.json').write_text(json.dumps(records,indent=2)+'\n')
(args.out/'files.json').write_text(json.dumps(files,indent=2)+'\n')
print('SECRET_COMMIT_FAILURE_OBSERVED',len({r['case']for r in records}),len(records))
