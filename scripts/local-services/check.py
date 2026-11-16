#!/usr/bin/env python3
"""Start local storage emulators, exercise them, and always stop them afterward."""
import argparse
import base64
from contextlib import closing, suppress
import hashlib
from importlib.metadata import version
import json
import logging
import os
from pathlib import Path
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

import boto3
from botocore.config import Config
from azure.storage.blob import BlobServiceClient
from moto.server import ThreadedMotoServer

REPO = Path(__file__).resolve().parents[2]
ACCOUNT = 'roastedduckfixture'
KEY = base64.b64encode(b'local-fixture-key-not-a-cloud-key').decode()
ACCESS = 'fixture-access'
SECRET = 'fixture-secret'


class Services:
    """Reusable fixture lifetime for future AWS/Azure extension differential tests."""
    def __init__(self, tools, out):
        self.tools, self.out = tools.resolve(), out.resolve()
        self.moto = self.azure = self.log = None
        self.active = False

    def __enter__(self):
        self.out.mkdir(parents=True, exist_ok=False)
        # Prevent setup from replacing tools underneath a running fixture.
        with (self.tools / 'active').open('x') as marker:
            marker.write(str(os.getpid()))
        self.active = True
        try:
            logging.getLogger('werkzeug').setLevel(logging.ERROR)
            self.moto = ThreadedMotoServer(ip_address='127.0.0.1', port=0, verbose=False)
            self.moto.start()
            _, port = self.moto.get_host_and_port()
            self.s3_endpoint = f'http://127.0.0.1:{port}'
            config, credentials = self.out / 'aws-config', self.out / 'aws-credentials'
            config.write_text('[profile roastedduck-fixture]\nregion=us-east-1\n')
            credentials.write_text(f'[roastedduck-fixture]\naws_access_key_id={ACCESS}\naws_secret_access_key={SECRET}\n')
            credentials.chmod(0o600)
            self.environment = {k: v for k, v in os.environ.items()
                                if not k.startswith(('AWS_', 'AZURE_'))}
            self.environment.update(AWS_CONFIG_FILE=str(config), AWS_SHARED_CREDENTIALS_FILE=str(credentials),
                                    AWS_PROFILE='roastedduck-fixture', AWS_REGION='us-east-1',
                                    AWS_EC2_METADATA_DISABLED='true', AWS_ENDPOINT_URL=self.s3_endpoint,
                                    NO_PROXY='localhost,127.0.0.1')
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                azure_port = sock.getsockname()[1]
            self.blob_endpoint = f'http://127.0.0.1:{azure_port}/{ACCOUNT}'
            self.azure_connection = (f'DefaultEndpointsProtocol=http;AccountName={ACCOUNT};'
                                     f'AccountKey={KEY};BlobEndpoint={self.blob_endpoint};')
            self.log = (self.out / 'azurite.log').open('w')
            env = dict(os.environ, AZURITE_ACCOUNTS=f'{ACCOUNT}:{KEY}', TMPDIR=str(self.tools / 'tmp'))
            self.azure = subprocess.Popen([
                str(self.tools / 'node/node_modules/.bin/azurite-blob'),
                '--blobHost', '127.0.0.1', '--blobPort', str(azure_port),
                '--location', str(self.out / 'blob-data'), '--silent', '--disableTelemetry'],
                env=env, stdout=self.log, stderr=subprocess.STDOUT)
            for _ in range(200):
                if self.azure.poll() is not None:
                    raise RuntimeError(f'Azurite exited; inspect {self.out / "azurite.log"}')
                try:
                    urllib.request.urlopen(self.blob_endpoint, timeout=0.2).close()
                    break
                except urllib.error.HTTPError:
                    break  # An authenticated service can reject a readiness request.
                except (OSError, urllib.error.URLError):
                    time.sleep(0.1)
            else:
                raise TimeoutError('Azurite did not become ready')
            self.s3 = boto3.client('s3', endpoint_url=self.s3_endpoint, region_name='us-east-1',
                                   aws_access_key_id=ACCESS, aws_secret_access_key=SECRET,
                                   config=Config(signature_version='s3v4',
                                                 s3={'addressing_style': 'path'},
                                                 retries={'total_max_attempts': 1}))
            self.blob = BlobServiceClient.from_connection_string(self.azure_connection,
                                                                 api_version='2023-11-03')
            return self
        except BaseException:
            self.__exit__(*sys.exc_info())
            raise

    def __exit__(self, *_):
        for name in ['s3', 'blob']:
            client = getattr(self, name, None)
            if client is not None:
                with suppress(Exception):
                    client.close()
        if self.azure is not None and self.azure.poll() is None:
            self.azure.terminate()
            try:
                self.azure.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.azure.kill()
                self.azure.wait(timeout=10)
        if self.moto is not None:
            self.moto.stop()
        if self.log is not None:
            self.log.close()
        if self.active:
            (self.tools / 'active').unlink(missing_ok=True)
            self.active = False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tools', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--cli', type=Path)
    parser.add_argument('--oracle', type=Path)
    parser.add_argument('--httpfs-extension', type=Path)
    parser.add_argument('--azure-extension', type=Path)
    parser.add_argument('--postgres-extension', type=Path)
    args = parser.parse_args()
    if args.oracle and not all([args.httpfs_extension, args.azure_extension, args.postgres_extension]):
        parser.error('Oracle mode requires all three extension paths')
    records = []

    def sql_check(engine, cli, sql, expected):
        result = subprocess.run([str(cli.resolve()), '-batch', '-bail', '-csv', '-noheader',
                                 '-init', '/dev/null', ':memory:'], input=sql,
                                capture_output=True, text=True, timeout=60, env=service.environment)
        passed = result.returncode == 0 and result.stdout.strip().splitlines() == expected
        records.append(dict(check=engine, passed=passed, sql=sql, exit=result.returncode,
                            stdout=result.stdout, stderr=result.stderr))
        (args.out / 'results.json').write_text(json.dumps(records, indent=2) + '\n')
        assert passed, f'{engine}: {result.stderr} {result.stdout}'

    def load(path):
        return "LOAD '" + str(path.resolve()).replace("'", "''") + "';\n"

    with Services(args.tools, args.out) as service:
        data = b'value\n1\n2\n3\n'
        service.s3.create_bucket(Bucket='fixture')
        service.s3.put_object(Bucket='fixture', Key='input.csv', Body=data)
        assert service.s3.get_object(Bucket='fixture', Key='input.csv')['Body'].read() == data
        assert service.s3.get_object(Bucket='fixture', Key='input.csv', Range='bytes=6-7')['Body'].read() == b'1\n'
        assert service.s3.list_objects_v2(Bucket='fixture')['KeyCount'] == 1
        records.append(dict(check='moto-s3-put-get-range-list', passed=True))
        with closing(boto3.client('sts', endpoint_url=service.s3_endpoint, region_name='us-east-1',
                                 aws_access_key_id=ACCESS, aws_secret_access_key=SECRET)) as sts:
            credentials = sts.assume_role(RoleArn='arn:aws:iam::123456789012:role/fixture',
                                          RoleSessionName='fixture')['Credentials']
            assert credentials['AccessKeyId'] and credentials['SessionToken']
        records.append(dict(check='moto-sts-assume-role', passed=True))
        container = service.blob.create_container('fixture')
        container.upload_blob('input.csv', data)
        assert container.download_blob('input.csv').readall() == data
        assert container.download_blob('input.csv', offset=6, length=2).readall() == b'1\n'
        assert [b.name for b in container.list_blobs()] == ['input.csv']
        records.append(dict(check='azurite-blob-put-get-range-list', passed=True))
        endpoint = service.s3_endpoint.removeprefix('http://')
        secret = (f"CREATE SECRET fixture (TYPE s3, KEY_ID '{ACCESS}', SECRET '{SECRET}', "
                  f"REGION 'us-east-1', ENDPOINT '{endpoint}', USE_SSL false, URL_STYLE 'path');\n")
        for engine, cli in [('rust', args.cli), ('cpp', args.oracle)]:
            if cli is None:
                continue
            startup = 'LOAD httpfs;\n' if engine == 'rust' else load(args.httpfs_extension)
            sql_check(engine + '-s3-read-copy', cli, startup + secret + f"""
SELECT sum(value) FROM read_csv('s3://fixture/input.csv', header=true, columns={{'value':'INTEGER'}});
COPY (SELECT 42 AS value) TO 's3://fixture/{engine}.parquet' (FORMAT parquet);
SELECT value FROM read_parquet('s3://fixture/{engine}.parquet');
""", ['true', '6', '42'])
        if args.oracle:
            sql_check('cpp-azure-read-copy', args.oracle, load(args.azure_extension) + f"""
CREATE SECRET fixture (TYPE azure, CONNECTION_STRING '{service.azure_connection}');
SELECT sum(value) FROM read_csv('az://fixture/input.csv', header=true, columns={{'value':'INTEGER'}});
COPY (SELECT 42 AS value) TO 'az://fixture/output.parquet' (FORMAT parquet);
SELECT value FROM read_parquet('az://fixture/output.parquet');
""", ['true', '6', '42'])
        container.delete_blob('input.csv')
        service.s3.delete_object(Bucket='fixture', Key='input.csv')
        assert not any(b.name == 'input.csv' for b in container.list_blobs())
        assert all(o['Key'] != 'input.csv' for o in service.s3.list_objects_v2(Bucket='fixture').get('Contents', []))
        records.append(dict(check='storage-delete', passed=True))
    if args.oracle:
        subprocess.run([sys.executable, str(REPO / 'scripts/postgres-tests/oracle_control.py'),
                        '--oracle', str(args.oracle), '--extension', str(args.postgres_extension),
                        '--out', str(args.out / 'postgres')], check=True)
        records.append(dict(check='cpp-postgres-contract', passed=True))
    report = dict(checks=records, services_stopped=True,
                  limitations=['ADLS Gen2 is not emulated by Azurite',
                               'Real IAM/Entra identity, cloud policies and network behavior unverified',
                               'Moto is not a substitute for independent SigV4 authentication tests'],
                  rust_azure_tested=False, rust_postgres_tested=False,
                  rust_aws_credential_chain_tested=False)
    identities = {}
    for name in ['cli', 'oracle', 'httpfs_extension', 'azure_extension', 'postgres_extension']:
        path = getattr(args, name)
        if path is not None:
            with path.open('rb') as stream:
                identities[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    report['sha256'] = identities
    report['tool_versions'] = {'moto': version('moto'), 'azure-storage-blob': version('azure-storage-blob'),
                              'azurite': json.loads((args.tools / 'node/node_modules/azurite/package.json').read_text())['version']}
    (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed_checks': len(records), 'services_stopped': True,
                      'scope': 'Local emulators and disposable PostgreSQL only'}))


if __name__ == '__main__':
    main()
