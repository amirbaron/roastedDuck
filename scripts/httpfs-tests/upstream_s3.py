#!/usr/bin/env python3
"""Run hash-verified upstream S3 SQL against an owned, pinned MinIO service."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile

from minio_fixture import Minio

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['slt', 'shim', 'source', 'core', 'extensions', 'tools', 'transport', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--file', action='append', required=True)
    parser.add_argument('--engine', choices=['cpp', 'rust', 'both'], default='both')
    parser.add_argument('--timeout', type=int, default=180)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    source = args.source.resolve()
    manifest = json.loads((ROOT / 'extensions/httpfs/upstream.json').read_text())
    archive = source.parent.parent / 'httpfs.tar.gz'
    assert hashlib.sha256(archive.read_bytes()).hexdigest() == manifest['archive_sha256']
    data_hashes = {}
    with tarfile.open(archive) as tar:
        for member in tar:
            if member.isfile() and '/data/' in member.name:
                relative = member.name.split('/', 1)[1]
                content = tar.extractfile(member).read()
                assert content == (source / relative).read_bytes()
                data_hashes[relative] = hashlib.sha256(content).hexdigest()
    for name in args.file:
        assert hashlib.sha256((source / name).read_bytes()).hexdigest() == manifest['sql_tests'][name]
    report = {'scope': 'Unchanged original SQL with owned MinIO; no live AWS claim',
              'data_hashes': data_hashes,
              'original_sql': {name: manifest['sql_tests'][name] for name in args.file},
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
            rows = []
            for index, name in enumerate(args.file):
                service.reset()
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
                with (case / 'run.log').open('w') as log:
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
