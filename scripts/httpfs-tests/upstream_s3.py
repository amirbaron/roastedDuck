#!/usr/bin/env python3
"""Run hash-verified HTTPFS SQL with owned fixtures or explicit public endpoints."""
import argparse
from collections import Counter
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile

from minio_fixture import Minio
from http_fixture import HttpServer

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['slt', 'shim', 'source', 'core', 'extensions', 'tools', 'transport', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--file', action='append', required=True)
    parser.add_argument('--engine', choices=['cpp', 'rust', 'both'], default='both')
    parser.add_argument('--timeout', type=int, default=180)
    parser.add_argument('--seeds', type=Path, help='Verified output from generate_s3_seeds.py')
    parser.add_argument('--public-network', action='store_true', help='Run public-service originals without child-local transport mapping')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    source = args.source.resolve()
    seeds = args.seeds.resolve() if args.seeds else None
    seed_manifest = json.loads((seeds / 'verification.json').read_text()) if seeds else None
    if seeds:
        from generate_s3_seeds import digest
        for name, entry in seed_manifest['files'].items():
            assert digest(seeds / name) == entry['sha256']
    manifest = json.loads((ROOT / 'extensions/httpfs/upstream.json').read_text())
    archive = source.parent.parent / 'httpfs.tar.gz'
    assert hashlib.sha256(archive.read_bytes()).hexdigest() == manifest['archive_sha256']
    data_hashes = {}
    with tarfile.open(archive) as tar:
        for member in tar:
            if member.isfile() and ('/data/' in member.name or member.name.split('/', 1)[1] == 'README.md'):
                relative = member.name.split('/', 1)[1]
                content = tar.extractfile(member).read()
                assert content == (source / relative).read_bytes()
                data_hashes[relative] = hashlib.sha256(content).hexdigest()
    for name in args.file:
        assert hashlib.sha256((source / name).read_bytes()).hexdigest() == manifest['sql_tests'][name]
        if args.public_network:
            assert 'require-env S3_TEST_SERVER_AVAILABLE' not in (source / name).read_text()
    report = {'scope': 'Unchanged original SQL against public endpoints' if args.public_network else 'Unchanged original SQL with owned MinIO; no live AWS claim',
              'data_hashes': data_hashes,
              'original_sql': {name: manifest['sql_tests'][name] for name in args.file},
              'seed_manifest': seed_manifest,
              'service_sha256': {name: hashlib.sha256((args.tools / name).read_bytes()).hexdigest()
                                 for name in ['minio', 'mc']},
              'sha256': {name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                         for name in ['slt', 'shim', 'transport']}}
    with Minio(args.tools, out / 'minio') as service:
        for engine in ['cpp', 'rust'] if args.engine == 'both' else [args.engine]:
            directory = out / engine
            directory.mkdir()
            reference = directory / 'reference'
            reference.mkdir()
            shutil.copytree(source / 'data', reference / 'data')
            # The invalid-CA original intentionally uses this existing non-PEM file.
            shutil.copyfile(source / 'README.md', reference / 'README.md')
            (reference / 'duckdb').symlink_to(args.core.resolve(), target_is_directory=True)
            (reference / 'extension').symlink_to(args.core.resolve() / 'extension', target_is_directory=True)
            for name in ['secrets', 'extensions', 'home']:
                (directory / name).mkdir()
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('AWS_', 'DUCKDB_', 'SLT_', 'AZURE_', 'HF_', 'HUGGING', 'DYLD_', 'ROASTEDDUCK_FIXTURE_'))
                   and k != 'LOCAL_EXTENSION_REPO'
                   and k.lower() not in ['http_proxy', 'https_proxy', 'all_proxy', 'no_proxy']}
            env.update(TMPDIR=str(directory), TZ='UTC', AWS_EC2_METADATA_DISABLED='true',
                       AWS_SHARED_CREDENTIALS_FILE=str(directory / 'home/credentials'),
                       AWS_CONFIG_FILE=str(directory / 'home/config'),
                       AWS_DEFAULT_REGION='eu-west-1', AWS_ACCESS_KEY_ID='minio_duckdb_user',
                       AWS_SECRET_ACCESS_KEY='minio_duckdb_user_password',
                       DUCKDB_S3_ENDPOINT='duckdb-minio.com:9000', DUCKDB_S3_USE_SSL='false',
                       S3_TEST_SERVER_AVAILABLE='1',
                       DYLD_INSERT_LIBRARIES=str(args.transport.resolve()),
                       ROASTEDDUCK_FIXTURE_S3_PORT=str(service.port),
                       SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),
                       SLT_FIXTURE_SECRET_DIRECTORY=str(directory / 'secrets'),
                       SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory / 'extensions'))
            if args.public_network:
                env.pop('DYLD_INSERT_LIBRARIES')
            rows = []
            for index, name in enumerate(args.file):
                service.reset()
                sql = (source / name).read_text()
                uploads = []
                if 'S3_SMALL_CSV_PRESIGNED_URL' in sql:
                    uploads.append((args.core.resolve() / 'data/csv/phonenumbers.csv', 'phonenumbers.csv', 'S3_SMALL_CSV_PRESIGNED_URL'))
                if 'S3_SMALL_PARQUET_PRESIGNED_URL' in sql:
                    uploads.append((args.core.resolve() / 'data/parquet-testing/glob/t1.parquet', 't1.parquet', 'S3_SMALL_PARQUET_PRESIGNED_URL'))
                if 'S3_ATTACH_DB' in sql:
                    assert seeds, 'Generate authentic attachment seeds before running this original'
                    uploads.append((seeds / 'attach.db', 'attach.db', 'S3_ATTACH_DB_PRESIGNED_URL'))
                    env['S3_ATTACH_DB'] = 's3://test-bucket/presigned/attach.db'
                if 'lineitem_sf1.db' in sql:
                    assert seeds, 'Generate authentic TPC-H database before running this original'
                    uploads.append((seeds / 'lineitem_sf1.db', 'lineitem_sf1.db', None))
                if 'S3_LARGE_PARQUET_PRESIGNED_URL' in sql:
                    assert seeds, 'Generate authentic TPC-H Parquet before running this original'
                    uploads.append((seeds / 'presigned-url-lineitem.parquet', 'lineitem_large.parquet', 'S3_LARGE_PARQUET_PRESIGNED_URL'))
                for local, remote, variable in uploads:
                    if local.is_relative_to(args.core.resolve()):
                        core_manifest = json.loads(args.core.with_name(args.core.name + '-manifest.json').read_text())
                        relative = str(local.relative_to(args.core.resolve()))
                        assert hashlib.sha256(local.read_bytes()).hexdigest() == core_manifest[relative]
                    target = 'owned/test-bucket/presigned/' + remote
                    service.mc('cp', local, target)
                    if variable:
                        shared = [json.loads(line) for line in service.mc('share', 'download', target).splitlines()]
                        assert len(shared) == 1 and shared[0]['status'] == 'success', shared
                        env[variable] = shared[0]['share']
                destination = reference / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes((source / name).read_bytes())
                case = directory / f'{index:03}'
                case.mkdir()
                command = [str(args.slt.resolve()), 'run', '--engine', engine, '--root', str(reference),
                           '--jobs', '1', '--timeout-secs', str(args.timeout), '--require', 'httpfs',
                           '--out', str(case / 'results.jsonl'), '--log-dir', str(case / 'logs'), name]
                if engine == 'cpp':
                    command += ['--shim', str(args.shim.resolve())]
                with ExitStack() as stack:
                    if 'require-env PYTHON_HTTP_SERVER_URL' in sql:
                        http = stack.enter_context(HttpServer(case / 'http'))
                        env.update(PYTHON_HTTP_SERVER_URL=http.url, PYTHON_HTTP_SERVER_DIR=str(http.directory))
                    log = stack.enter_context((case / 'run.log').open('w'))
                    run = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                                         timeout=args.timeout + 60)
                results = [json.loads(line) for line in (case / 'results.jsonl').read_text().splitlines()]
                assert len(results) == 1
                rows.extend(results)
                print(engine, name, results[0]['status'], flush=True)
                report[engine] = dict(last_exit=run.returncode, rows=rows,
                                      counts=dict(Counter(r['status'] for r in rows)))
                (out / 'progress.json').write_text(json.dumps(report, indent=2) + '\n')
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    if any(row['status'] != 'pass' for engine in ['cpp', 'rust'] if engine in report
           for row in report[engine]['rows']):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
