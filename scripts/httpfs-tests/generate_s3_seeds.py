#!/usr/bin/env python3
"""Generate upstream S3 attachment/presigned fixtures with the pinned C++ oracle."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def quote(path):
    return str(path).replace("'", "''")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['oracle', 'core', 'core_manifest', 'tpch', 'out']:
        parser.add_argument('--' + name.replace('_', '-'), type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    (out / 'tmp').mkdir()
    sql_path = 'test/sql/storage_version/generate_storage_version.sql'
    sql = (args.core / sql_path).read_bytes()
    assert hashlib.sha256(sql).hexdigest() == json.loads(args.core_manifest.read_text())[sql_path]
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('DUCKDB_', 'DYLD_', 'AWS_'))}
    env['TMPDIR'] = str(out / 'tmp')
    version = subprocess.check_output([str(args.oracle.resolve()), '-noheader', '-list', '-c',
                                       'SELECT version()'], env=env, text=True).strip()
    assert version == 'v1.5.6', version
    records = []

    def run(name, database, statements):
        (out / (name + '.sql')).write_text(statements)
        with (out / (name + '.log')).open('w') as log:
            result = subprocess.run([str(args.oracle.resolve()), '-batch', '-bail', str(out / database)],
                                    input=statements, text=True, stdout=log,
                                    stderr=subprocess.STDOUT, cwd=out, env=env, timeout=600)
        records.append(dict(name=name, exit=result.returncode,
                            sql_sha256=hashlib.sha256(statements.encode()).hexdigest()))
        (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        assert result.returncode == 0, name
        print(name, 'generated', flush=True)

    prefix = f"SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; SET temp_directory='{quote(out / 'tmp')}';\n"
    run('storage-version', 'attach.db', prefix + sql.decode())
    run('tpch-sf1', 'lineitem_sf1.db', prefix +
        f"LOAD '{quote(args.tpch.resolve())}'; CALL dbgen(sf=1);\n"
        f"COPY lineitem TO '{quote(out / 'presigned-url-lineitem.parquet')}' (FORMAT PARQUET); CHECKPOINT;\n")
    report = dict(generator='Pinned C++ DuckDB 1.5.6; upstream storage SQL and DBGEN(sf=1)',
                  oracle_sha256=digest(args.oracle), tpch_sha256=digest(args.tpch),
                  storage_sql_sha256=hashlib.sha256(sql).hexdigest(), commands=records,
                  files={name: dict(sha256=digest(out / name), size=(out / name).stat().st_size)
                         for name in ['attach.db', 'lineitem_sf1.db', 'presigned-url-lineitem.parquet']})
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
