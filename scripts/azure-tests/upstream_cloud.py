#!/usr/bin/env python3
"""Run pinned Azure cloud originals using owned token, TLS, account and proxy fixtures."""
import argparse
from collections import Counter
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

from cloud_fixture import BLOB_ACCOUNT, DFS_ACCOUNT, PUBLIC_ACCOUNT, HOSTS, CloudFixture, ConnectProxy
from identity_control import TOKEN, tls_material

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_FILES = [f'test/sql/cloud/{name}.test' for name in
                 ['basic','cli_auth','spn_auth','env_spn_auth','access_token_auth','unauthenticated',
                  'glob','hierarchical_namespace','network_test','vfs_ops','azure_writes']]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','cli','oracle','source','extension','out']:
        parser.add_argument('--'+name, type=Path, required=True)
    parser.add_argument('--engine', choices=['cpp','rust','both'], default='both')
    parser.add_argument('--file', action='append')
    parser.add_argument('--all', action='store_true', help='Run all 27 unchanged original Azure files')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    manifest = json.loads((ROOT/'extensions/azure/upstream.json').read_text())
    if args.all and args.file:
        parser.error('--all and --file are mutually exclusive')
    files = list(manifest['sql_tests']) if args.all else (args.file or DEFAULT_FILES)
    extensions = out/'extensions'
    extensions.mkdir()
    (extensions/'azure.duckdb_extension').symlink_to(args.extension.resolve())
    context, ca, _, cert = tls_material(out, HOSTS)
    executable_dir = out/'bin'
    executable_dir.mkdir()
    az = executable_dir/'az'
    az.write_text('#!/usr/bin/env python3\nimport json,time\n'+
                  f'print(json.dumps({{"accessToken":{TOKEN!r},"expires_on":int(time.time())+3600}}))\n')
    az.chmod(0o755)
    report = {}
    for engine in ['cpp','rust']:
        if args.engine not in ['both',engine]:
            continue
        target = out/engine
        reference = target/'reference'
        for name in files:
            content = (args.source/name).read_bytes()
            assert hashlib.sha256(content).hexdigest() == manifest['sql_tests'][name]
            destination = reference/name
            destination.parent.mkdir(parents=True,exist_ok=True)
            destination.write_bytes(content)
        shutil.copytree(args.source/'data',reference/'data')
        hashes = {p.relative_to(reference/'data').as_posix():hashlib.sha256(p.read_bytes()).hexdigest()
                  for p in (reference/'data').rglob('*') if p.is_file()}
        with ExitStack() as stack:
            fixture = stack.enter_context(CloudFixture(context,cert,reference/'data'))
            proxy = stack.enter_context(ConnectProxy(fixture.server.server_port))
            proxies = [proxy]
            if any('/proxy/' in file for file in files):
                proxies += [stack.enter_context(ConnectProxy(fixture.server.server_port,3128)),
                            stack.enter_context(ConnectProxy(fixture.server.server_port,3129,authenticated=True))]
            base = fixture.endpoint.rsplit('/',1)[0]
            env = {k:v for k,v in os.environ.items() if not k.startswith(('AWS_','AZURE_','AZ_','ABFSS_','PUBLIC_AZ_','SLT_','IDENTITY_','MSI_','IMDS_'))
                   and k.lower() not in ['http_proxy','https_proxy','no_proxy','all_proxy'] and k!='LOCAL_EXTENSION_REPO'}
            env.update(AZURE_AUTHORITY_HOST=base,AZURE_POD_IDENTITY_AUTHORITY_HOST=base,
                       AZURE_TENANT_ID='tenant',AZURE_CLIENT_ID='fixture-client',AZURE_CLIENT_SECRET='fixture-secret',
                       AZURE_ACCESS_TOKEN=TOKEN,AZURE_AUTH_ENV='1',AZ_CLI_LOGGED_IN='1',
                       AZ_STORAGE_ACCOUNT=BLOB_ACCOUNT,ABFSS_STORAGE_ACCOUNT=DFS_ACCOUNT,PUBLIC_AZ_STORAGE_ACCOUNT=PUBLIC_ACCOUNT,
                       AZ_DATA_DIR='duckdblabs-data/common/azure_data',ABFSS_DATA_DIR='duckdblabs-data/common/azure_data',
                       AZ_TEMP_DIR='duckdblabs-write-testing/extension/azure',ABFSS_TEMP_DIR='duckdblabs-write-testing/extension/azure',
                       AZURE_STORAGE_CONNECTION_STRING=fixture.connection,DUCKDB_AZURE_PUBLIC_CONTAINER_AVAILABLE='1',
                       HTTP_PROXY_RUNNING='1',HTTP_PROXY=proxy.url,HTTPS_PROXY=proxy.url,http_proxy=proxy.url,https_proxy=proxy.url,
                       NO_PROXY='',no_proxy='',CURL_CA_INFO=str(ca),SLT_AZURE_TRANSPORT='curl',
                       SLT_ORACLE_EXTENSION_DIRECTORY=str(extensions),PATH=str(executable_dir)+os.pathsep+os.environ['PATH'],TMPDIR=str(target))
            rows=[]
            for index,name in enumerate(files):
                fixture.case=name
                file_env=dict(env)
                if name.endswith('/test_data_integrity.test'):
                    secrets=target/'persisted-secrets'
                    secrets.mkdir(exist_ok=True)
                    secret_path=str(secrets).replace("'","''")
                    extension_path=str(args.extension.resolve()).replace("'","''")
                    startup=f"LOAD '{extension_path}';" if engine=='cpp' else 'LOAD azure;'
                    sql=startup+f"SET secret_directory='{secret_path}'; CREATE OR REPLACE PERSISTENT SECRET fixture(TYPE azure,CONNECTION_STRING '{fixture.connection}');"
                    seed=subprocess.run([str((args.oracle if engine=='cpp' else args.cli).resolve()),'-batch','-bail','-init','/dev/null',':memory:'],input=sql,text=True,capture_output=True,env=file_env,timeout=30)
                    (target/'persistent-seed.json').write_text(json.dumps(dict(exit=seed.returncode,stdout=seed.stdout,stderr=seed.stderr),indent=2)+'\n')
                    assert seed.returncode==0
                    file_env.update(SLT_SECRET_DIRECTORY=str(secrets),DUCKDB_AZURE_PERSISTENT_SECRET_AVAILABLE='1',ENABLE_DATA_INTEGRITY='1')
                result=target/f'{index}.jsonl'
                command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--timeout-secs','600' if name.endswith('_slow') else '240',
                         '--require','azure','--require','parquet','--out',str(result),'--log-dir',str(target/'logs'),name]
                if engine=='cpp':
                    command+=['--shim',str(args.shim.resolve())]
                with (target/f'{index}.log').open('w') as log:
                    subprocess.run(command,env=file_env,stdout=log,stderr=subprocess.STDOUT)
                rows.extend(json.loads(line) for line in result.read_text().splitlines())
                (target/'progress.json').write_text(json.dumps(rows,indent=2)+'\n')
            (target/'requests.json').write_text(json.dumps(fixture.requests,indent=2)+'\n')
            (target/'storage-requests.json').write_text(json.dumps(fixture.storage_requests,indent=2)+'\n')
            (target/'token-requests.json').write_text(json.dumps(fixture.tokens,indent=2)+'\n')
            (target/'proxy-requests.json').write_text(json.dumps([p.requests for p in proxies],indent=2)+'\n')
            report[engine]=dict(rows=rows,counts=dict(Counter(r['status'] for r in rows)),data_sha256=hashes,
                                auth=dict(Counter(r['authentication'] for r in fixture.requests)),
                                invalid_authorization=sum(not r['valid_authorization'] for r in fixture.requests),
                                fixture_error_count=fixture.server_error_count+sum(p.error_count for p in proxies),
                                fixture_errors=fixture.server_errors+[error for p in proxies for error in p.errors])
    report['sha256']={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['slt','shim','extension']}
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({e:dict(counts=r['counts'],nonpasses=[dict(file=x['file'],status=x['status'],line=x.get('failure',{}).get('line')) for x in r['rows'] if x['status']!='pass']) for e,r in report.items() if e!='sha256'}))
    if any(row['status']!='pass' for e,r in report.items() if e!='sha256' for row in r['rows']):
        raise SystemExit(1)
    if any(r['fixture_error_count'] for e,r in report.items() if e!='sha256'):
        raise SystemExit(1)


if __name__=='__main__':
    main()
