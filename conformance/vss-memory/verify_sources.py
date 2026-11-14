#!/usr/bin/env python3
"""Verify source provenance only; does not run SQL or grant engine acceptance.

Usage: python3 conformance/vss-memory/verify_sources.py /path/to/pinned/duckdb-vss
"""
import hashlib
import json
from pathlib import Path
import sys


def digest(data):
    return hashlib.sha256(data).hexdigest()


def block(path, first, last):
    lines = path.read_text().splitlines(keepends=True)
    return ''.join(lines[first - 1:last]).rstrip('\n') + '\n'


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    upstream = Path(sys.argv[1]).resolve()
    root = Path(__file__).resolve().parent
    manifest = json.loads((root / 'source-manifest.json').read_text())
    for original in manifest['originals']:
        assert digest((upstream / original['path']).read_bytes()) == original['sha256'], original['path']
    copied = 0
    authored = 0
    for supplement in manifest['supplements']:
        destination = root / supplement['path']
        assert digest(destination.read_bytes()) == supplement['sha256'], supplement['path']
        for record in supplement['records']:
            if 'source' in record:
                original = block(upstream / record['source'], record['start_line'], record['end_line'])
                actual = block(destination, record['output_start_line'], record['output_start_line'] + len(original.splitlines()) - 1)
                assert actual == original, (supplement['path'], record['record'])
                assert digest(original.encode()) == record['block_sha256']
                alias = record.get('also_matches')
                if alias:
                    other = block(upstream / alias['source'], alias['start_line'], alias['end_line'])
                    assert other == original
                    assert digest(other.encode()) == alias['block_sha256']
                copied += 1
            else:
                actual = block(destination, record['start_line'], record['end_line'])
                assert digest(actual.encode()) == record['block_sha256']
                authored += 1
            sql = actual.split('\n', 1)[1].split('\n----\n')[0].rstrip('\n') + '\n'
            assert digest(sql.encode()) == record['sql_sha256']
    print(f'SOURCE_ONLY: 6 unchanged originals, {copied} unchanged copied records, {authored} authored records; no SQL executed')


if __name__ == '__main__':
    main()
