#!/usr/bin/env python3
"""Exercise statement/database cache boundaries against real owned S3 versions."""
import argparse
import ctypes
import hashlib
import json
from pathlib import Path

from minio_fixture import Minio
from s3_version_control import API, quote


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['library', 'package', 'tools', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    records = []
    with Minio(args.tools, out / 'minio') as service:
        api = API(args.library, out / 'secrets')
        first = api.con
        second = ctypes.c_void_p()
        try:
            assert api.lib.duckdb_connect(api.db, ctypes.byref(second)) == 0

            def query(label, sql, expected=None):
                record = api.query(sql)
                record['label'] = label
                records.append(record)
                (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
                assert record['status'] == 0, record
                if expected is not None:
                    assert record['rows'] == expected, record
                return record['rows']

            for connection in [first, second]:
                api.con = connection
                query('load', f"LOAD '{quote(args.package.resolve())}'")
                query('configure', f"SET s3_endpoint='127.0.0.1:{service.port}'; SET s3_use_ssl=false; SET s3_url_style='path'; SET s3_region='eu-west-1'; SET s3_access_key_id='minio_duckdb_user'; SET s3_secret_access_key='minio_duckdb_user_password'; SET enable_external_file_cache=false; SET s3_version_id_pinning=true")
            path = 's3://test-bucket/cache-boundary.parquet'
            api.con = first
            query('seed', f"COPY (SELECT 1 AS value) TO '{path}' (FORMAT PARQUET)")
            query('global on', 'SET enable_http_metadata_cache=true')
            query('capture v1', f"SELECT * FROM '{path}'", [['1']])
            api.con = second
            query('disable shared cache for overwrite', 'SET enable_http_metadata_cache=false')
            query('local overwrite', f"COPY (SELECT 2 AS value) TO '{path}' (FORMAT PARQUET)")
            query('local sees v2', f"SELECT * FROM '{path}'", [['2']])
            api.con = first
            query('restore shared cache after overwrite', 'SET enable_http_metadata_cache=true')
            query('global still pins v1', f"SELECT * FROM '{path}'", [['1']])
            query('global off', 'SET enable_http_metadata_cache=false')
            query('local sees v2 after switch', f"SELECT * FROM '{path}'", [['2']])
            api.con = second
            query('another local overwrite', f"COPY (SELECT 3 AS value) TO '{path}' (FORMAT PARQUET)")
            api.con = first
            query('local expires between queries', f"SELECT * FROM '{path}'", [['3']])
            query('restore global', 'SET enable_http_metadata_cache=true')
            query('global survives local reads', f"SELECT * FROM '{path}'", [['1']])
            query('global overwrite invalidates', f"COPY (SELECT 4 AS value) TO '{path}' (FORMAT PARQUET)")
            query('global captures v4', f"SELECT * FROM '{path}'", [['4']])
            query('duplicate response headers parse', "CALL enable_logging('HTTP')")
            query('logged read', f"SELECT * FROM '{path}'", [['4']])
            query('valid user agent', "SELECT count(*) FROM duckdb_logs_parsed('HTTP') WHERE request.headers['User-Agent'] IS NULL OR NOT starts_with(request.headers['User-Agent'], 'duckdb/')", [['0']])
        finally:
            api.con = first
            if second.value:
                api.lib.duckdb_disconnect(ctypes.byref(second))
            api.close()
    (out / 'verification.json').write_text(json.dumps(dict(records=records,
        sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                for name in ['library', 'package']}), indent=2) + '\n')
    print(f'{len(records)} cache boundary records passed', flush=True)


if __name__ == '__main__':
    main()
