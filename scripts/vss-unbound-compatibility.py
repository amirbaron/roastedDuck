#!/usr/bin/env python3
"""Verify clean C++ HNSW databases remain readable with opaque, unbound indexes.

This tests read compatibility and rejected writes, not cross-engine HNSW persistence.
Requires an official DuckDB 1.5.6 oracle and its pinned b833341 VSS artifact.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def literal(value):
    return "'" + str(value).replace("'", "''") + "'"


def decode(raw):
    text = raw.decode()
    decoder = json.JSONDecoder()
    rows = []
    while text.strip():
        result, end = decoder.raw_decode(text.lstrip())
        rows.append(result)
        text = text.lstrip()[end:]
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['oracle-cli', 'rust-cli', 'vss-extension', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--load-rust-vss', action='store_true')
    args = parser.parse_args()
    for name in ['oracle_cli', 'rust_cli', 'vss_extension', 'out']:
        setattr(args, name, getattr(args, name).resolve())
    args.out.mkdir(parents=True, exist_ok=False)
    empty_init = args.out / 'empty-init.sql'
    empty_init.write_text('')
    footer = args.vss_extension.read_bytes()[-512:]
    fields = [footer[n * 32:(n + 1) * 32].rstrip(b'\0').decode() for n in range(8)]
    assert fields[3:6] == ['CPP', 'b833341', 'v1.5.6'], fields
    assert digest(args.oracle_cli) != digest(args.rust_cli), 'Oracle and Rust must be different binaries'
    report = {'passed': False, 'scope': 'clean persisted C++ HNSW read/unbound compatibility only',
              'load_rust_vss': args.load_rust_vss, 'records': [],
              'identities': {name: {'path': str(getattr(args, name)), 'sha256': digest(getattr(args, name))}
                             for name in ['oracle_cli', 'rust_cli', 'vss_extension']}, 'footer': fields}

    def save():
        (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')

    def run(engine, name, database, sql, readonly=False, rejected=False):
        prefix = args.out / f'{len(report["records"]):03d}-{engine}-{name}'
        setup = 'SET autoinstall_known_extensions=false;\nSET threads=1;\n'
        if engine == 'oracle':
            setup += f'LOAD {literal(args.vss_extension)};\n'
        elif args.load_rust_vss:
            setup += 'LOAD vss;\n'
        query = setup + sql + '\n'
        prefix.with_suffix('.sql').write_text(query)
        cli = args.oracle_cli if engine == 'oracle' else args.rust_cli
        command = [str(cli), '-json', '-bail', '-init', str(empty_init)]
        if readonly:
            command.append('-readonly')
        command.append(str(database))
        result = subprocess.run(command, input=query.encode(), capture_output=True, cwd=args.out, timeout=60)
        prefix.with_suffix('.stdout').write_bytes(result.stdout)
        prefix.with_suffix('.stderr').write_bytes(result.stderr)
        report['records'].append({'engine': engine, 'name': name, 'exit': result.returncode,
                                  'readonly': readonly, 'raw_prefix': prefix.name})
        save()
        if rejected:
            assert result.returncode != 0 and b'remains unbound' in result.stderr, (name, result.stderr)
            return []
        assert result.returncode == 0, (name, result.stderr)
        return decode(result.stdout)

    try:
        identity = run('oracle', 'identity', ':memory:', 'SELECT version() AS version;')
        assert identity == [[{'version': 'v1.5.6'}]], identity
        original = args.out / 'oracle-hnsw.db'
        run('oracle', 'create', original, '''SET hnsw_enable_experimental_persistence=true;
CREATE TABLE vectors(id INTEGER, vec FLOAT[3]);
INSERT INTO vectors VALUES (1,[0,0,0]),(2,[1,0,0]),(3,[2,0,0]),(4,[3,1,0]);
CREATE INDEX ids ON vectors(id);
CREATE INDEX neighbors ON vectors USING HNSW(vec);
CHECKPOINT;
''')
        inspection = run('oracle', 'index-proof', original,
                         'SELECT index_name,count FROM pragma_hnsw_index_info();', readonly=True)
        assert inspection == [[{'index_name': 'neighbors', 'count': 4}]], inspection
        queries = '''SELECT id, vec FROM vectors ORDER BY id;
SELECT id FROM vectors WHERE id >= 2 ORDER BY id;
SELECT id FROM vectors WHERE array_distance(vec,[1,0,0]::FLOAT[3]) <= 1 ORDER BY id;
SELECT id FROM vectors ORDER BY array_distance(vec,[1,0,0]::FLOAT[3]), id LIMIT 2;
SELECT index_name, sql FROM duckdb_indexes() ORDER BY index_name;
'''
        expected = run('oracle', 'read-reference', original, queries, readonly=True)
        original_hash = digest(original)
        for readonly in [True, False]:
            path = args.out / ('readonly.db' if readonly else 'readwrite-open.db')
            shutil.copyfile(original, path)
            before = digest(path)
            actual = run('rust', 'read', path, queries, readonly=readonly)
            assert actual == expected, {'expected': expected, 'actual': actual}
            assert digest(path) == before, 'A read changed the database file'
        for name, sql in [('insert', 'INSERT INTO vectors VALUES (5,[4,0,0]);'),
                          ('update', 'UPDATE vectors SET vec=[9,0,0] WHERE id=1;'),
                          ('delete', 'DELETE FROM vectors WHERE id=1;'),
                          ('checkpoint', 'CHECKPOINT;')]:
            path = args.out / (name + '.db')
            shutil.copyfile(original, path)
            before = digest(path)
            run('rust', name, path, sql, rejected=True)
            assert digest(path) == before, name + ' changed the database file'
            assert run('oracle', name + '-reopen', path, queries, readonly=True) == expected
            assert run('oracle', name + '-graph', path,
                       'SELECT index_name,count FROM pragma_hnsw_index_info();', readonly=True) == inspection
        assert digest(original) == original_hash
        report['passed'] = True
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        save()
    print(json.dumps({'passed': report['passed'], 'records': len(report['records']), 'report': str(args.out / 'verification.json')}))


if __name__ == '__main__':
    main()
