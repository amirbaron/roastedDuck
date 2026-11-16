#!/usr/bin/env python3
"""S3 error texts for missing objects, rejected credentials and regions, on owned MinIO."""
import argparse
import hashlib
import json
from pathlib import Path

from minio_fixture import Minio
from s3_version_control import API, quote

VALID = ("SET s3_region='eu-west-1'; SET s3_access_key_id='minio_duckdb_user'; "
         "SET s3_secret_access_key='minio_duckdb_user_password'")


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
        try:
            for sql in [f"LOAD '{quote(args.package.resolve())}'",
                        f"SET s3_endpoint='127.0.0.1:{service.port}'; SET s3_use_ssl=false; SET s3_url_style='path'; "
                        'SET enable_external_file_cache=false; SET http_retries=0; ' + VALID,
                        "COPY (SELECT 1 AS value) TO 's3://test-bucket/errors/present.parquet' (FORMAT PARQUET)",
                        "COPY (SELECT 1 AS value) TO 's3://test-bucket/errors/present.csv' (FORMAT CSV)"]:
                result = api.query(sql)
                assert result['status'] == 0, result
            present = 's3://test-bucket/errors/present.parquet'
            cases = [
                ('present', VALID, f"SELECT * FROM '{present}'"),
                ('missing-parquet', VALID, "SELECT * FROM 's3://test-bucket/errors/missing.parquet'"),
                ('missing-blob', VALID, "SELECT size FROM read_blob('s3://test-bucket/errors/missing.bin')"),
                ('missing-csv', VALID, "SELECT * FROM read_csv('s3://test-bucket/errors/missing.csv')"),
                ('missing-bucket', VALID, "SELECT * FROM 's3://owned-absent-bucket/errors/present.parquet'"),
                ('missing-glob', VALID, "SELECT count(*) FROM glob('s3://owned-absent-bucket/errors/*.parquet')"),
                ('wrong-secret', "SET s3_secret_access_key='owned_wrong_secret'", f"SELECT * FROM '{present}'"),
                ('wrong-secret-blob', '', "SELECT size FROM read_blob('s3://test-bucket/errors/present.csv')"),
                ('wrong-secret-glob', '', "SELECT count(*) FROM glob('s3://test-bucket/errors/*.parquet')"),
                ('wrong-secret-write', '', "COPY (SELECT 2 AS value) TO 's3://test-bucket/errors/denied.parquet' (FORMAT PARQUET)"),
                ('unknown-key', VALID + "; SET s3_access_key_id='owned_unknown_user'", f"SELECT * FROM '{present}'"),
                ('no-credentials', "SET s3_access_key_id=''; SET s3_secret_access_key=''", f"SELECT * FROM '{present}'"),
                ('no-credentials-glob', '', "SELECT count(*) FROM glob('s3://test-bucket/errors/*.parquet')"),
                ('no-credentials-write', '', "COPY (SELECT 2 AS value) TO 's3://test-bucket/errors/denied.csv' (FORMAT CSV)"),
                ('wrong-region', VALID + "; SET s3_region='us-west-1'", f"SELECT * FROM '{present}'"),
                ('wrong-region-glob', '', "SELECT count(*) FROM glob('s3://test-bucket/errors/*.parquet')"),
                ('no-region', VALID + "; SET s3_region=''", f"SELECT * FROM '{present}'"),
                ('secret-wrong', VALID + "; CREATE SECRET owned_wrong (TYPE s3, KEY_ID 'minio_duckdb_user', SECRET 'owned_wrong_secret', "
                 f"REGION 'eu-west-1', ENDPOINT '127.0.0.1:{service.port}', USE_SSL false, URL_STYLE 'path', SCOPE 's3://test-bucket/errors')",
                 f"SELECT * FROM '{present}'"),
                ('recovered', 'DROP SECRET owned_wrong; ' + VALID, f"SELECT * FROM '{present}'"),
            ]
            for label, setup, sql in cases:
                if setup:
                    configured = api.query(setup)
                    assert configured['status'] == 0, configured
                record = dict(label=label, **api.query(sql))
                records.append(record)
                (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        finally:
            api.close()
    (out / 'verification.json').write_text(json.dumps(dict(records=records, stopped=True,
        sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                for name in ['library', 'package']}), indent=2) + '\n')
    print(f'{len(records)} S3 error records captured', flush=True)


if __name__ == '__main__':
    main()
