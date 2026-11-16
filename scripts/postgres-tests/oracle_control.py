#!/usr/bin/env python3
"""Run a bounded PostgreSQL connector contract on a disposable local PG cluster.

This qualifies the reference fixture only unless --rust-cli is also supplied.
The server and all generated files belong to the fresh --out directory.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--oracle', required=True, type=Path)
    parser.add_argument('--extension', required=True, type=Path)
    parser.add_argument('--rust-cli', type=Path)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    initdb, pg_ctl = shutil.which('initdb'), shutil.which('pg_ctl')
    if not initdb or not pg_ctl:
        parser.error('PostgreSQL initdb and pg_ctl are required for the local fixture')
    data = args.out / 'data'
    password = args.out / 'fixture-password'
    # This is an isolated, loopback-only disposable test account, not a user secret.
    password.write_text('roastedduck-fixture-only\n')
    password.chmod(0o600)
    env = dict(os.environ)
    env.update(PGPASSWORD='roastedduck-fixture-only')
    with (args.out / 'initdb.log').open('w') as log:
        subprocess.run([initdb, '-D', str(data), '-U', 'postgres', '--no-locale', '-E', 'UTF8',
                        '--auth-local=trust', '--auth-host=scram-sha-256',
                        '--pwfile', str(password)], stdout=log, stderr=subprocess.STDOUT,
                       check=True, timeout=60)
    password.unlink()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    with (data / 'postgresql.conf').open('a') as config:
        config.write(f"\nlisten_addresses='127.0.0.1'\nport={port}\nunix_socket_directories=''\n"
                     "max_connections=20\nshared_buffers='16MB'\nlog_min_messages=warning\n")
    records = []
    failures = []
    started = False
    try:
        subprocess.run([pg_ctl, '-D', str(data), '-l', str(args.out / 'postgres.log'),
                        '-w', 'start'], check=True, capture_output=True, text=True, timeout=60)
        started = True
        connection = f'host=127.0.0.1 port={port} dbname=postgres user=postgres'
        for name, cli in [('cpp', args.oracle), ('rust', args.rust_cli)]:
            if cli is None:
                continue
            load = ("LOAD '" + str(args.extension.resolve()).replace("'", "''") + "';\n"
                    if name == 'cpp' else 'LOAD postgres;\n')
            # Separate tables prevent a failed candidate from altering oracle evidence.
            table = f'contract_{name}'
            sql = load + f"""
ATTACH '{connection}' AS remote (TYPE postgres);
CREATE TABLE remote.{table}(id INTEGER, label VARCHAR);
INSERT INTO remote.{table} VALUES (1,'first'),(2,'second');
SELECT 'read=' || sum(id)::VARCHAR FROM remote.{table};
BEGIN;
INSERT INTO remote.{table} VALUES (3,'rolled back');
ROLLBACK;
SELECT 'rollback=' || count(*)::VARCHAR FROM remote.{table};
UPDATE remote.{table} SET label='updated' WHERE id=2;
SELECT 'updated=' || label FROM remote.{table} WHERE id=2;
SELECT 'query=' || total::VARCHAR FROM postgres_query('remote', 'SELECT sum(id) AS total FROM {table}');
SELECT 'join=' || count(*)::VARCHAR FROM remote.{table} r JOIN (VALUES (2)) local(id) USING(id);
DETACH remote;
"""
            result = subprocess.run([str(cli.resolve()), '-batch', '-bail', '-csv', '-noheader',
                                     '-init', '/dev/null', ':memory:'], input=sql, env=env,
                                    text=True, capture_output=True, timeout=60)
            expected = ['read=3', 'rollback=2', 'updated=updated', 'query=3', 'join=1']
            passed = result.returncode == 0 and result.stdout.strip().splitlines() == expected
            records.append(dict(engine=name, sql=sql, exit=result.returncode, stdout=result.stdout,
                                stderr=result.stderr, passed=passed,
                                cli_sha256=hashlib.sha256(cli.read_bytes()).hexdigest()))
            if not passed:
                failures.append(name)
    finally:
        if started or (data / 'postmaster.pid').exists():
            subprocess.run([pg_ctl, '-D', str(data), '-m', 'fast', '-w', 'stop'],
                           check=True, capture_output=True, text=True, timeout=60)
        (args.out / 'results.json').write_text(json.dumps(records, indent=2) + '\n')
    report = dict(oracle_passed=any(r['engine'] == 'cpp' and r['passed'] for r in records),
                  rust_tested=args.rust_cli is not None,
                  rust_passed=any(r['engine'] == 'rust' and r['passed'] for r in records),
                  postgres=subprocess.check_output([pg_ctl, '--version'], text=True).strip(),
                  extension_sha256=hashlib.sha256(args.extension.read_bytes()).hexdigest(),
                  server_stopped=not (data / 'postmaster.pid').exists(),
                  scope='Basic local PostgreSQL contract; not full upstream or cloud acceptance')
    (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    if failures:
        raise SystemExit('Contract failed: ' + ', '.join(failures))


if __name__ == '__main__':
    main()
