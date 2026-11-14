#!/usr/bin/env python3
"""Paired Azure auth, escaped-name, write and SQL metadata controls on Azurite."""
import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess

from azure.storage.blob import ContainerSasPermissions, generate_container_sas
from upstream_blob import services


def quote(value):
    return str(value).replace("'", "''")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['cli', 'oracle', 'extension', 'tools', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    rows = []
    with services.Services(args.tools, out / 'services') as fixture:
        container = fixture.blob.create_container('fixture')
        names = ['input.csv', 'spaces and ünicode.csv', 'symbols & # percent%.csv']
        for name in names:
            container.upload_blob(name, b'value\n1\n2\n3\n')
        sas = generate_container_sas(services.ACCOUNT, 'fixture', account_key=services.KEY,
                                    permission=ContainerSasPermissions(read=True, list=True),
                                    expiry=datetime.now(timezone.utc) + timedelta(minutes=20))
        connection = fixture.azure_connection
        sas_connection = f'AccountName={services.ACCOUNT};BlobEndpoint={fixture.blob_endpoint};SharedAccessSignature={sas}'
        cases = [(f'name-{index}', f"CREATE SECRET s(TYPE azure, CONNECTION_STRING '{connection}'); SELECT sum(value) FROM 'az://fixture/{quote(name)}';", 0)
                 for index, name in enumerate(names)]
        cases += [
            ('sas', f"CREATE SECRET s(TYPE azure, CONNECTION_STRING '{sas_connection}'); SELECT sum(value) FROM 'az://fixture/input.csv';", 0),
            ('bad-key', f"CREATE SECRET s(TYPE azure, CONNECTION_STRING '{connection.replace(services.KEY, 'YWJjZA==')}'); SELECT * FROM 'az://fixture/input.csv';", 1),
            ('secret-config', "CREATE SECRET s(TYPE azure, CONNECTION_STRING 'fake', HTTP_PROXY 'proxy', PROXY_PASSWORD 'hidden'); SELECT secret_string FROM duckdb_secrets();", 0),
            ('secret-chain', "CREATE SECRET s(TYPE azure, PROVIDER credential_chain, CHAIN 'env;cli', ACCOUNT_NAME 'fixture'); SELECT secret_string FROM duckdb_secrets();", 0),
            ('secret-managed', "CREATE SECRET s(TYPE azure, PROVIDER managed_identity, CLIENT_ID 'client', OBJECT_ID 'object'); SELECT secret_string FROM duckdb_secrets();", 0),
            ('secret-spn', "CREATE SECRET s(TYPE azure, PROVIDER service_principal, TENANT_ID 'tenant', CLIENT_ID 'client', CLIENT_SECRET 'hidden'); SELECT secret_string FROM duckdb_secrets();", 0),
            ('secret-token', "CREATE SECRET s(TYPE azure, PROVIDER access_token, ACCESS_TOKEN 'hidden'); SELECT secret_string FROM duckdb_secrets();", 0),
            ('invalid-option', "CREATE SECRET s(TYPE azure, UNKNOWN 3);", 1),
            ('sas-logs', f"CREATE SECRET s(TYPE azure, CONNECTION_STRING '{sas_connection}'); CALL enable_logging('HTTP', storage='memory'); SELECT sum(value) FROM 'az://fixture/input.csv'; SELECT count(*)>0, bool_and(NOT contains(request.url, 'sig=') OR contains(request.url, 'sig=REDACTED')) FROM duckdb_logs_parsed('HTTP');", 0),
        ]
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(('AZURE_', 'AWS_')) and k.lower() not in ['http_proxy', 'https_proxy', 'no_proxy', 'all_proxy']}
        env['TMPDIR'] = str(args.tools.resolve() / 'tmp')
        for engine, cli in [('cpp', args.oracle), ('rust', args.cli)]:
            startup = 'SET allow_persistent_secrets=false; '
            startup += f"LOAD '{quote(args.extension.resolve())}'; " if engine == 'cpp' else 'INSTALL azure; LOAD azure; '
            for name, sql, expected_exit in cases:
                result = subprocess.run([str(cli.resolve()), '-batch', '-bail', '-csv', '-noheader', '-init', '/dev/null', ':memory:'],
                                        input=startup + sql, text=True, capture_output=True, env=env, timeout=60)
                rows.append(dict(engine=engine, case=name, exit=result.returncode, stdout=result.stdout,
                                 stderr=result.stderr, expected_exit=expected_exit))
        checks = []
        for name, _, expected_exit in cases:
            cpp, rust = [r for r in rows if r['case'] == name]
            passed = (cpp['exit'] != 0) == (expected_exit != 0) and (rust['exit'] != 0) == (expected_exit != 0)
            if expected_exit == 0:
                passed = passed and cpp['stdout'] == rust['stdout']
            elif name == 'bad-key':
                passed = passed and all('AuthorizationFailure' in row['stderr'] for row in [cpp, rust])
            else:
                passed = passed and all("Unknown parameter 'unknown'" in row['stderr'] for row in [cpp, rust])
            checks.append(dict(case=name, passed=passed))
    report = dict(checks=checks, rows=rows, sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                                                  for name in ['cli', 'oracle', 'extension']})
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(checks))
    if not all(check['passed'] for check in checks):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
