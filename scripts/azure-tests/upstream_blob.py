#!/usr/bin/env python3
"""Run unchanged Azure Blob originals against isolated Azurite storage per engine."""
import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('local_services', ROOT / 'scripts/local-services/check.py')
services = importlib.util.module_from_spec(spec)
spec.loader.exec_module(services)
FILES = [f'test/sql/{name}.test' for name in
         ['azure_secret', 'azure_scope_and_full_path', 'azure_writes', 'azure_glob']]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['slt', 'shim', 'source', 'extension', 'tools', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--file', action='append', help='Original file path relative to pinned source')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    manifest = json.loads((ROOT / 'extensions/azure/upstream.json').read_text())
    files = args.file or FILES
    extensions = out / 'extensions'
    extensions.mkdir()
    (extensions / 'azure.duckdb_extension').symlink_to(args.extension.resolve())
    report = {}
    for engine in ['cpp', 'rust']:
        with services.Services(args.tools, out / engine) as fixture:
            reference = out / engine / 'reference'
            for name in files:
                data = (args.source / name).read_bytes()
                assert hashlib.sha256(data).hexdigest() == manifest['sql_tests'][name]
                target = reference / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            shutil.copytree(args.source / 'data', reference / 'data')
            hashes = {}
            for container in ['testing-private', 'testing-public', 'writes']:
                client = fixture.blob.create_container(container)
                if container != 'writes':
                    for path in sorted((reference / 'data').rglob('*')):
                        if path.is_file():
                            name = path.relative_to(reference / 'data').as_posix()
                            data = path.read_bytes()
                            hashes[name] = hashlib.sha256(data).hexdigest()
                            client.upload_blob(name, data)
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('AWS_', 'AZURE_', 'SLT_', 'AZ_'))
                   and k.lower() not in ['http_proxy', 'https_proxy', 'all_proxy', 'no_proxy']
                   and k != 'LOCAL_EXTENSION_REPO'}
            env.update(AZURE_STORAGE_CONNECTION_STRING=fixture.azure_connection,
                       AZ_TEMP_DIR='writes', AZ_STORAGE_ACCOUNT=services.ACCOUNT,
                       SLT_ORACLE_EXTENSION_DIRECTORY=str(extensions), TMPDIR=str(args.tools.resolve() / 'tmp'))
            result = out / engine / 'results.jsonl'
            command = [str(args.slt.resolve()), 'run', '--engine', engine, '--root', str(reference),
                       '--jobs', '1', '--timeout-secs', '180', '--require', 'azure', '--require', 'parquet',
                       '--out', str(result), '--log-dir', str(out / engine / 'logs'), *files]
            if engine == 'cpp':
                command += ['--shim', str(args.shim.resolve())]
            with (out / engine / 'run.log').open('w') as log:
                run = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
            rows = [json.loads(line) for line in result.read_text().splitlines()]
            report[engine] = dict(exit=run.returncode, rows=rows, data_sha256=hashes,
                                  counts=dict(Counter(row['status'] for row in rows)))
    report['sha256'] = {name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                        for name in ['slt', 'shim', 'extension']}
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({e: dict(counts=report[e]['counts'], nonpasses=[
        dict(file=r['file'], status=r['status'], line=r.get('failure', {}).get('line'))
        for r in report[e]['rows'] if r['status'] != 'pass']) for e in ['cpp', 'rust']}))
    if any(row['status'] != 'pass' for e in ['cpp', 'rust'] for row in report[e]['rows']):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
