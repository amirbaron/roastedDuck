#!/usr/bin/env python3
"""Paired CA/glob diagnostics and cold/warm disabled-filesystem state controls."""
import argparse
import hashlib
import json
from pathlib import Path

from http_fixture import HttpServer
from s3_version_control import API, quote


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['library', 'package', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    (out / 'invalid.pem').write_text('owned non-certificate fixture\n')
    records = []
    with HttpServer(out / 'http') as server:
        (server.directory / 'a.csv').write_text('a\n42\n')
        cases = {}
        for implementation in ['curl', 'httplib']:
            for name in ['invalid.pem', 'absent.pem']:
                cases[implementation + '-' + name] = [
                    (f"SET httpfs_client_implementation='{implementation}'; SET http_retries=0; SET enable_server_cert_verification=true; SET ca_cert_file='{quote(out / name)}'", False),
                    (f"FROM read_blob('{server.url.replace('http:', 'https:')}/a.csv')", True),
                ]
        cases['glob'] = [
            (f"FROM read_parquet('{server.url}/*.parquet')", True),
            ('SET allow_asterisks_in_http_paths=true', False),
            (f"FROM read_parquet('{server.url}/*.parquet')", True),
            ('SET allow_asterisks_in_http_paths=false', False),
            (f"FROM read_parquet('{server.url}/*.parquet')", True),
        ]
        cases['prepared-visibility'] = [
            ('PREPARE names AS SELECT name FROM duckdb_secrets() ORDER BY name', False),
            ("CREATE SECRET fixture_secret(TYPE s3, KEY_ID 'owned', SECRET 'owned')", False),
            ('EXECUTE names', False),
            ('DROP SECRET fixture_secret', False),
            ('EXECUTE names', False),
        ]
        cases['prepared-cold'] = [
            ('PREPARE names AS SELECT name FROM duckdb_secrets()', False),
            ("SET disabled_filesystems='LocalFileSystem'", False),
            ('EXECUTE names', True),
            ('EXECUTE names', True),
        ]
        for mode in ['cold', 'verified-cold', 'warm', 'persistence-disabled']:
            statements = []
            if mode == 'verified-cold':
                statements.append(('PRAGMA enable_verification', False))
            if mode == 'warm':
                statements.append(('SELECT * FROM duckdb_secrets()', False))
            if mode == 'persistence-disabled':
                statements.append(('SET allow_persistent_secrets=false', False))
            blocked = mode in ['cold', 'verified-cold']
            statements += [
                ("SET disabled_filesystems='LocalFileSystem'", False),
                (f"FROM read_csv_auto('{server.url}/a.csv')", blocked),
                ('SELECT * FROM duckdb_secrets()', blocked),
                ("SET disabled_filesystems='LocalFileSystem,HTTPFileSystem'", False),
                (f"FROM read_csv_auto('{server.url}/a.csv')", True),
            ]
            cases[mode] = statements
        for case, statements in cases.items():
            directory = out / case
            directory.mkdir()
            api = API(args.library, directory / 'secrets')
            try:
                loaded = api.query(f"LOAD '{quote(args.package.resolve())}'")
                assert loaded['status'] == 0, loaded
                for sql, expect_error in statements:
                    record = dict(case=case, **api.query(sql))
                    records.append(record)
                    (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                    assert bool(record['status']) == expect_error, record
            finally:
                api.close()
    (out / 'verification.json').write_text(json.dumps(dict(records=records,
        sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                for name in ['library', 'package']}, stopped=True), indent=2) + '\n')
    print(f'{len(records)} diagnostic/state records passed', flush=True)


if __name__ == '__main__':
    main()
