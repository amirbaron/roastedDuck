#!/usr/bin/env python3
"""Compare every pinned Snowball vocabulary word against DuckDB's FTS oracle.

Build/sign crates/snowball/examples/stem-lines separately; this script never builds.
Input corpus: snowballstem/snowball-data@a32eee945f8a71cda47a88f660910a59dacf6955.
Expected outputs ALWAYS come from C++ DuckDB 1.5.6, not corpus output.txt files.
Each algorithm runs serially; successful scratch outputs are removed after hashing.
"""
import argparse
import gzip
import hashlib
import itertools
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent
DATA_PIN = 'a32eee945f8a71cda47a88f660910a59dacf6955'
BOUNDARIES = ['', 'information', 'information information', 'information ', '\0information',
              'information\0s', 'INFORMATION', 'Connection', '🦆', 'a\nword', 'word\tword',
              'café', 'cafe\u0301', 'привет', 'العربية', 'தமிழ்', 'हिन्दी', 'šuma',
              '\u200b', '\r\n', "can't", 'i' * 4096 + 'nformation']


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def vocabulary(data, name):
    path = data / name / 'voc.txt'
    if path.exists():
        with path.open('rb') as file:
            yield from (line.rstrip(b'\r\n') for line in file)
    else:
        with gzip.open(path.with_suffix('.txt.gz'), 'rb') as file:
            yield from (line.rstrip(b'\r\n') for line in file)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['oracle', 'extension', 'rust', 'data', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--algorithms', nargs='*', help='Focused diagnostic subset; omitted means all 27')
    parser.add_argument('--limit', type=int, help='Diagnostic cap, not full vocabulary acceptance')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    provenance = json.loads((ROOT / 'crates/snowball/provenance.json').read_text())
    algorithms = args.algorithms or [row['name'] for row in provenance['algorithms']]
    report = {'data_commit': DATA_PIN, 'oracle_sha256': sha(args.oracle), 'extension_sha256': sha(args.extension),
              'rust_sha256': sha(args.rust), 'limit': args.limit, 'algorithm_subset': args.algorithms,
              'cases': []}
    identity = subprocess.run([str(args.oracle.resolve()), '-csv', '-noheader', '-c',
        f"LOAD {quote(args.extension.resolve())}; SELECT version(), extension_version FROM duckdb_extensions() WHERE extension_name='fts';"], capture_output=True, text=True, check=True)
    report['oracle_identity'] = identity.stdout.strip()
    assert identity.stdout.startswith('v1.5.6,'), identity.stdout
    cases = [(name, name, True) for name in algorithms]
    cases += [(alias, canonical, False) for alias, canonical in provenance['aliases'] if alias != canonical and canonical in algorithms]
    if args.algorithms is None:
        cases += [('none', 'porter', False), ('porter\0ignored', 'porter', False)]
    for case_number, (alias, canonical, full) in enumerate(cases):
        base = args.out / f'{case_number:03d}-{canonical}'
        input_path = base.with_suffix('.input.tsv')
        rust_input = base.with_suffix('.rust-input.tsv')
        oracle_output = base.with_suffix('.oracle.tsv')
        rust_output = base.with_suffix('.rust.txt')
        words = vocabulary(args.data, canonical) if full else iter(())
        if args.limit is not None:
            words = itertools.islice(words, args.limit)
        words = itertools.chain(words, (word.encode() for word in BOUNDARIES))
        count = 0
        with input_path.open('w') as inputs, rust_input.open('w') as rust_inputs:
            for count, word in enumerate(words, 1):
                # SQL VARCHAR input must be valid UTF-8; malformed bytes are tested in Rust runtime tests.
                word.decode('utf-8')
                inputs.write(f'{count}\t{word.hex()}\n')
                rust_inputs.write(f'{alias}\t{word.hex()}\n')
        algorithm_sql = f"decode(from_hex('{alias.encode().hex()}'))"
        sql = f"""LOAD {quote(args.extension.resolve())}; SET threads=1;
SELECT id, hex(stem(decode(from_hex(word)), {algorithm_sql}))
FROM read_csv({quote(input_path.resolve())}, columns={{'id':'BIGINT','word':'VARCHAR'}}, delim='\\t', header=false, nullstr='NO_NULL_SENTINEL');"""
        with oracle_output.open('w') as out:
            p = subprocess.run([str(args.oracle.resolve()), '-list', '-separator', '\t', '-noheader', '-c', sql], stdout=out, stderr=subprocess.PIPE, text=True)
        assert p.returncode == 0, (alias, p.stderr)
        with rust_input.open('r') as source, rust_output.open('w') as out:
            p = subprocess.run([str(args.rust.resolve())], stdin=source, stdout=out, stderr=subprocess.PIPE, text=True)
        assert p.returncode == 0, (alias, p.stderr)
        differences = []
        mismatch_count = 0
        seen = 0
        with oracle_output.open() as oracle, rust_output.open() as rust:
            for seen, (expected, actual) in enumerate(itertools.zip_longest(oracle, rust), 1):
                parts = expected.rstrip('\n').split('\t') if expected else []
                assert len(parts) == 2 and int(parts[0]) == seen, (alias, seen, expected)
                actual = actual.rstrip('\n') if actual is not None else None
                if parts[1] != actual:
                    mismatch_count += 1
                    if len(differences) < 20:
                        differences.append({'row': seen, 'oracle': parts[1], 'rust': actual})
        assert seen == count, (alias, seen, count)
        record = {'algorithm': alias, 'canonical': canonical, 'full_vocabulary': full and args.limit is None,
                  'count': count, 'mismatches': mismatch_count, 'examples': differences,
                  'input_sha256': sha(input_path), 'oracle_sha256': sha(oracle_output), 'rust_sha256': sha(rust_output)}
        report['cases'].append(record)
        (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({k: record[k] for k in ['algorithm', 'count', 'mismatches']}), flush=True)
        if mismatch_count == 0:
            for path in [input_path, rust_input, oracle_output, rust_output]:
                path.unlink()
    assert all(case['mismatches'] == 0 for case in report['cases']), 'Snowball differential mismatches retained'


if __name__ == '__main__':
    main()
