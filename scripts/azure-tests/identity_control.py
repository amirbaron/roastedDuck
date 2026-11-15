#!/usr/bin/env python3
"""Paired Azure identity acquisition against owned TLS/token/storage services."""
import argparse
import base64
from datetime import datetime, timedelta, timezone
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys
import time
import urllib.parse

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.x509.oid import NameOID
from dfs_fixture import ACCOUNT, DfsFixture

TOKEN = 'fixture-access-token'
WORKLOAD = 'fixture-workload-assertion'
RESOURCE_ID = '/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/fixture/providers/Microsoft.ManagedIdentity/userAssignedIdentities/fixture'


def tls_material(out):
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'Local Azure test fixture')])
    now = datetime.now(timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key())
            .serial_number(x509.random_serial_number()).not_valid_before(now - timedelta(days=1))
            .not_valid_after(now + timedelta(days=2))
            .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
            .add_extension(x509.SubjectAlternativeName([x509.IPAddress(ipaddress.ip_address('127.0.0.1'))]), critical=False)
            .sign(key, hashes.SHA256()))
    certificate = out / 'fixture-ca.pem'
    certificate.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    private = out / 'fixture-key.pem'
    private.write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    private.chmod(0o600)
    combined = out / 'fixture-identity.pem'
    combined.write_bytes(certificate.read_bytes() + private.read_bytes())
    combined.chmod(0o600)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certificate, private)
    return context, certificate, combined, cert


class IdentityFixture(DfsFixture):
    def __init__(self, context, cert):
        super().__init__(context, TOKEN)
        self.cert = cert
        self.tokens = []
        self.case = ''

    def handle_identity(self, handler, parsed, body):
        if parsed.path.startswith('/' + ACCOUNT + '/'):
            return False
        fields = {k: v[-1] for k, v in urllib.parse.parse_qs(
            body.decode() if handler.command == 'POST' else parsed.query, keep_blank_values=True).items()}
        valid = False
        method = ''
        try:
            if parsed.path == '/tenant/oauth2/v2.0/token':
                assert fields['grant_type'] == 'client_credentials'
                assert fields['client_id'] == 'fixture-client'
                assert fields['scope'] == 'https://storage.azure.com/.default'
                if 'client_secret' in fields:
                    method = 'client_secret'
                    assert fields['client_secret'] == 'fixture-secret'
                else:
                    assert fields['client_assertion_type'] == 'urn:ietf:params:oauth:client-assertion-type:jwt-bearer'
                    assertion = fields['client_assertion']
                    if assertion == WORKLOAD:
                        method = 'workload'
                    else:
                        method = 'certificate'
                        header, payload, signature = assertion.split('.')
                        decode = lambda s: base64.urlsafe_b64decode(s + '=' * (-len(s) % 4))
                        claims = json.loads(decode(payload))
                        metadata = json.loads(decode(header))
                        assert metadata['alg'] == 'RS256'
                        fingerprint = self.cert.fingerprint(hashes.SHA1())
                        assert decode(metadata['x5t']) == fingerprint
                        assert metadata['kid'] == fingerprint.hex().upper()
                        assert claims['aud'] == self.endpoint.rsplit('/', 1)[0] + parsed.path
                        assert claims['iss'] == claims['sub'] == 'fixture-client'
                        assert abs(claims['nbf'] - time.time()) < 60 and claims['exp'] - claims['nbf'] == 600
                        self.cert.public_key().verify(decode(signature), (header + '.' + payload).encode(), padding.PKCS1v15(), hashes.SHA256())
                valid = True
            else:
                assert fields['resource'] == 'https://storage.azure.com'
                method = parsed.path.strip('/')
                if parsed.path == '/msi2019':
                    assert handler.headers['X-IDENTITY-HEADER'] == 'fixture-identity-header'
                    assert fields['api-version'] == '2019-08-01'
                elif parsed.path == '/msi2017':
                    assert handler.headers['secret'] == 'fixture-msi-secret'
                    assert fields['api-version'] == '2017-09-01'
                elif parsed.path == '/cloudshell':
                    assert handler.command == 'POST' and handler.headers['Metadata'] == 'true'
                elif parsed.path == '/metadata/identity/oauth2/token':
                    assert handler.headers['Metadata'] == 'true' and fields['api-version'] == '2018-02-01'
                    assert self.case != 'default-fallback'
                elif parsed.path == '/arc':
                    assert handler.headers['Metadata'] == 'true'
                    self.tokens.append(dict(case=self.case, method=handler.command, path=parsed.path,
                                            provider='arc-challenge', valid=True, fields=sorted(fields)))
                    handler.respond(401, headers={'WWW-Authenticate':'Basic realm=/var/opt/azcmagent/tokens/fixture.key'})
                    return True
                else:
                    raise ValueError('unexpected identity path')
                if self.case == 'managed-object':
                    assert fields['principal_id'] == 'fixture-object'
                if self.case in ['managed-2019', 'managed-imds']:
                    assert fields['client_id'] == 'fixture-client'
                if self.case == 'managed-2017':
                    assert fields['clientid'] == 'fixture-client'
                if self.case == 'managed-resource':
                    assert fields['mi_res_id'] == RESOURCE_ID
                valid = True
        except Exception:
            valid = False
        self.tokens.append(dict(case=self.case, method=handler.command, path=parsed.path,
                                provider=method, valid=valid, fields=sorted(fields)))
        if valid:
            handler.respond(200, json.dumps(dict(access_token=TOKEN, expires_in=3600, token_type='Bearer')).encode(),
                            {'Content-Type': 'application/json'})
        else:
            handler.respond(400, b'{"error":"invalid_client","error_description":"local fixture rejected credentials"}',
                            {'Content-Type': 'application/json'})
        return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['cli', 'oracle', 'extension', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--engine', choices=['cpp', 'rust', 'both'], default='both')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    context, ca, certificate, cert = tls_material(out)
    workload = out / 'workload-token'
    workload.write_text(WORKLOAD)
    executable_dir = out / 'bin'
    executable_dir.mkdir()
    command = executable_dir / 'az'
    command.write_text('#!/usr/bin/env python3\nimport json,os,time,pathlib,sys\n'
                       f'root=pathlib.Path({str(out)!r})\n'
                       'case=json.loads((root/"active-case.json").read_text())\n'
                       'with (root/"cli.jsonl").open("a") as f: f.write(json.dumps(dict(case=case,args=sys.argv[1:],env=dict(os.environ)))+"\\n")\n'
                       f'token={{"accessToken":{TOKEN!r},"expires_on":int(time.time())+3600}}\n'
                       'if case["name"]=="cli-legacy-expired": token.pop("expires_on"); token["expiresOn"]=time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(time.time()-60))\n'
                       'print(json.dumps(token))\n'
                       'sys.exit(3 if case["name"]=="cli-nonzero" else 0)\n')
    command.chmod(0o755)
    rows = []
    for engine, cli in [('cpp', args.oracle), ('rust', args.cli)]:
        if args.engine not in ['both', engine]:
            continue
        with IdentityFixture(context, cert) as fixture, IdentityFixture(None, cert) as metadata:
            base = fixture.endpoint.rsplit('/', 1)[0]
            metadata_base = metadata.endpoint.rsplit('/', 1)[0]
            endpoint = fixture.endpoint.removeprefix('https://127.')
            common = f"TYPE azure, ACCOUNT_NAME '127', ENDPOINT '{endpoint}', "
            principal = "PROVIDER service_principal, TENANT_ID 'tenant', CLIENT_ID 'fixture-client', "
            environment = dict(AZURE_TENANT_ID='tenant', AZURE_CLIENT_ID='fixture-client', AZURE_CLIENT_SECRET='fixture-secret')
            cases = [
                ('access-token', "PROVIDER access_token, ACCESS_TOKEN 'fixture-access-token'", {}, True),
                ('client-secret', principal + "CLIENT_SECRET 'fixture-secret'", {}, True),
                ('environment', "PROVIDER credential_chain, CHAIN 'env'", environment, True),
                ('workload', "PROVIDER credential_chain, CHAIN 'workload_identity'", dict(AZURE_TENANT_ID='tenant', AZURE_CLIENT_ID='fixture-client', AZURE_FEDERATED_TOKEN_FILE=str(workload)), True),
                ('cli', "PROVIDER credential_chain, CHAIN 'cli'", {}, True),
                ('cli-nonzero', "PROVIDER credential_chain, CHAIN 'cli'", {}, True),
                ('cli-legacy-expired', "PROVIDER credential_chain, CHAIN 'cli'", {}, True),
                ('default-selected-cli', "PROVIDER credential_chain", dict(AZURE_TOKEN_CREDENTIALS='AzureCliCredential'), True),
                ('default-selected-env', "PROVIDER credential_chain", dict(environment, AZURE_TOKEN_CREDENTIALS='EnvironmentCredential'), True),
                ('env-before-cli', "PROVIDER credential_chain, CHAIN 'env;cli'", environment, True),
                ('default-fallback', "PROVIDER credential_chain", {}, True),
                ('managed-2019', "PROVIDER managed_identity, CLIENT_ID 'fixture-client'", dict(IDENTITY_ENDPOINT=metadata_base+'/msi2019', IDENTITY_HEADER='fixture-identity-header'), True),
                ('managed-object', "PROVIDER managed_identity, OBJECT_ID 'fixture-object'", dict(IDENTITY_ENDPOINT=metadata_base+'/msi2019', IDENTITY_HEADER='fixture-identity-header'), True),
                ('managed-resource', f"PROVIDER managed_identity, RESOURCE_ID '{RESOURCE_ID}'", dict(IDENTITY_ENDPOINT=metadata_base+'/msi2019', IDENTITY_HEADER='fixture-identity-header'), True),
                ('managed-2017', "PROVIDER managed_identity, CLIENT_ID 'fixture-client'", dict(MSI_ENDPOINT=metadata_base+'/msi2017', MSI_SECRET='fixture-msi-secret'), True),
                ('managed-cloudshell', "PROVIDER managed_identity", dict(MSI_ENDPOINT=metadata_base+'/cloudshell'), True),
                ('managed-imds', "PROVIDER managed_identity, CLIENT_ID 'fixture-client'", {}, True),
                ('managed-ignores-custom-ca', "PROVIDER managed_identity", dict(IDENTITY_ENDPOINT=base+'/msi2019', IDENTITY_HEADER='fixture-identity-header'), False),
                ('certificate', principal + f"CLIENT_CERTIFICATE_PATH '{certificate}'", {}, True),
                ('environment-certificate', "PROVIDER credential_chain, CHAIN 'env'", dict(AZURE_TENANT_ID='tenant', AZURE_CLIENT_ID='fixture-client', AZURE_CLIENT_CERTIFICATE_PATH=str(certificate)), True),
                ('wrong-secret', principal + "CLIENT_SECRET 'wrong-secret'", {}, False),
                ('invalid-chain', "PROVIDER credential_chain, CHAIN 'env;invalid'", environment, False),
                ('multiple-managed-ids', "PROVIDER managed_identity, CLIENT_ID 'client', OBJECT_ID 'object'", {}, False),
                ('insecure-authority', principal + "CLIENT_SECRET 'fixture-secret'", dict(AZURE_AUTHORITY_HOST=base.replace('https:', 'http:')), False),
            ]
            if sys.platform == 'darwin':
                cases.append(('managed-arc-platform', "PROVIDER managed_identity", dict(IDENTITY_ENDPOINT=metadata_base+'/arc', IMDS_ENDPOINT='fixture'), False))
            for name, options, extra, success in cases:
                fixture.case = metadata.case = name
                (out / 'active-case.json').write_text(json.dumps(dict(engine=engine,name=name)))
                env = {k:v for k,v in os.environ.items() if not k.startswith(('AZURE_', 'AWS_', 'IDENTITY_', 'MSI_', 'IMDS_'))
                       and k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
                env.update(AZURE_AUTHORITY_HOST=base, AZURE_POD_IDENTITY_AUTHORITY_HOST=metadata_base,
                           CURL_CA_INFO=str(ca), PATH=str(executable_dir)+os.pathsep+os.environ['PATH'],
                           TMPDIR=str(out))
                env.update(extra)
                startup = 'SET allow_persistent_secrets=false; '
                startup += f"LOAD '{args.extension.resolve()}'; " if engine == 'cpp' else 'LOAD azure; '
                sql = startup + f"SET azure_transport_option_type='curl'; CREATE SECRET s({common}{options}); SELECT sum(value) FROM 'az://fixture/input.csv'; SELECT sum(value) FROM 'abfss://fixture/input.csv';"
                before = len(fixture.tokens)
                metadata_before = len(metadata.tokens)
                run = subprocess.run([str(cli.resolve()), '-batch', '-bail', '-csv', '-noheader', '-init', '/dev/null', ':memory:'],
                                     input=sql, text=True, capture_output=True, env=env, timeout=60)
                passed = run.returncode == 0 and run.stdout.strip().splitlines() == ['true','6','6'] if success else run.returncode != 0
                errors = {'managed-ignores-custom-ca':'certificate', 'wrong-secret':'400',
                          'invalid-chain':'Unknown credential provider found: invalid',
                          'multiple-managed-ids':'Only 1 of client_id', 'insecure-authority':'Authority host must be a TLS protected'}
                errors['managed-arc-platform'] = 'Unsupported OS. Arc supports only Linux and Windows.'
                if not success:
                    passed = passed and errors[name].lower() in run.stderr.lower()
                rows.append(dict(engine=engine, case=name, passed=passed, exit=run.returncode,
                                 stdout=run.stdout, stderr=run.stderr, tokens=fixture.tokens[before:]+metadata.tokens[metadata_before:]))
                (out / 'progress.json').write_text(json.dumps(rows, indent=2)+'\n')
            (out / f'{engine}-storage-requests.json').write_text(json.dumps(fixture.requests, indent=2)+'\n')
            assert all(r['valid_authorization'] for r in fixture.requests)
    cli_rows = [json.loads(line) for line in (out/'cli.jsonl').read_text().splitlines()]
    pairs = []
    if args.engine == 'both':
        for name, _, _, _ in cases:
            cpp, rust = [r for r in rows if r['case']==name]
            tokens_match = cpp['tokens'] == rust['tokens']
            cpp_cli = [dict(args=r['args'],env=r['env']) for r in cli_rows if r['case']==dict(engine='cpp',name=name)]
            rust_cli = [dict(args=r['args'],env=r['env']) for r in cli_rows if r['case']==dict(engine='rust',name=name)]
            pairs.append(dict(case=name, tokens_match=tokens_match, cli_matches=cpp_cli==rust_cli))
    report = dict(rows=rows, pairs=pairs, sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest()
                                    for name in ['cli','oracle','extension']})
    (out/'verification.json').write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps({e:dict(passed=sum(r['passed'] for r in rows if r['engine']==e),
                             failed=[r['case'] for r in rows if r['engine']==e and not r['passed']])
                      for e in sorted({r['engine'] for r in rows})}))
    if not all(r['passed'] for r in rows) or not all(p['tokens_match'] and p['cli_matches'] for p in pairs):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
