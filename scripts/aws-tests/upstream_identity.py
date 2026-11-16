#!/usr/bin/env python3
"""Run unchanged AWS web-identity/RDS originals against owned STS/SSO fixtures.

macOS C++ providers use fixed HTTPS endpoints. The test-only transport shim
redirects their DNS/socket traffic and trusts only this fixture's CA for those
names. It preserves TLS verification; no DNS or keychain settings are changed.
Rust uses its SDK endpoint override to the same handler on loopback HTTP.
"""
import argparse
from datetime import datetime,timedelta,timezone
import hashlib
import http.server
import json
import os
from pathlib import Path
import ssl
import subprocess
import threading
import urllib.parse
from cryptography import x509
from cryptography.hazmat.primitives import hashes,serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID,ExtendedKeyUsageOID

ROOT=Path(__file__).resolve().parents[2]
FILES=['test/sql/env/aws_secret_web_identity_env.test','test/sql/aws_secret_rds.test']
ROLE='arn:aws:iam::984506134640:role/duckdb-irsa-test-role'
TOKEN='fixture-web-token'
HOSTS=['sts.eu-west-1.amazonaws.com','sts.us-east-1.amazonaws.com','sts.amazonaws.com','portal.sso.eu-west-1.amazonaws.com']

def certificate(out,negative=None):
    now=datetime.now(timezone.utc);key=rsa.generate_private_key(public_exponent=65537,key_size=2048);ca_key=rsa.generate_private_key(public_exponent=65537,key_size=2048)
    ca_name=x509.Name([x509.NameAttribute(NameOID.COMMON_NAME,'Owned AWS fixture CA')])
    ca=x509.CertificateBuilder().subject_name(ca_name).issuer_name(ca_name).public_key(ca_key.public_key()).serial_number(x509.random_serial_number()).not_valid_before(now-timedelta(days=1)).not_valid_after(now+timedelta(days=2)).add_extension(x509.BasicConstraints(ca=True,path_length=0),critical=True).add_extension(x509.KeyUsage(digital_signature=False,content_commitment=False,key_encipherment=False,data_encipherment=False,key_agreement=False,key_cert_sign=True,crl_sign=True,encipher_only=None,decipher_only=None),critical=True).sign(ca_key,hashes.SHA256())
    cert=x509.CertificateBuilder().subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME,HOSTS[0])])).issuer_name(ca_name).public_key(key.public_key()).serial_number(x509.random_serial_number()).not_valid_before(now-timedelta(days=1)).not_valid_after(now+timedelta(days=2)).add_extension(x509.SubjectAlternativeName([x509.DNSName(h) for h in (['wrong.fixture'] if negative=='hostname' else HOSTS)]),critical=False).add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]),critical=False).add_extension(x509.KeyUsage(digital_signature=True,content_commitment=False,key_encipherment=True,data_encipherment=False,key_agreement=False,key_cert_sign=False,crl_sign=False,encipher_only=None,decipher_only=None),critical=True).sign(ca_key,hashes.SHA256())
    if negative=='ca':
        wrong_key=rsa.generate_private_key(public_exponent=65537,key_size=2048)
        ca=x509.CertificateBuilder().subject_name(ca_name).issuer_name(ca_name).public_key(wrong_key.public_key()).serial_number(x509.random_serial_number()).not_valid_before(now-timedelta(days=1)).not_valid_after(now+timedelta(days=2)).add_extension(x509.BasicConstraints(ca=True,path_length=0),critical=True).sign(wrong_key,hashes.SHA256())
    (out/'ca.der').write_bytes(ca.public_bytes(serialization.Encoding.DER));(out/'ca.pem').write_bytes(ca.public_bytes(serialization.Encoding.PEM));(out/'server.pem').write_bytes(cert.public_bytes(serialization.Encoding.PEM));(out/'server.key').write_bytes(key.private_bytes(serialization.Encoding.PEM,serialization.PrivateFormat.PKCS8,serialization.NoEncryption()));(out/'server.key').chmod(0o600)
    context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(out/'server.pem',out/'server.key');return context

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','oracle','source','extensions','transport','out']:parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--sso-control',choices=['valid','missing','expired','malformed','empty-token','invalid-expiry','rejected-token'])
    parser.add_argument('--transport-negative',choices=['ca','hostname'])
    parser.add_argument('--engine',choices=['cpp','rust','both'],default='both')
    args=parser.parse_args()
    if args.transport_negative and args.engine!='cpp':parser.error('negative transport controls require --engine cpp')
    files=['sso-control.test'] if args.sso_control else (FILES[:1] if args.transport_negative else FILES)
    out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);requests=[];report={}
    expiry=datetime.now(timezone.utc)+timedelta(hours=2);expiration=expiry.strftime('%Y-%m-%dT%H:%M:%SZ')
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def send(self,status,payload,content_type):
            self.send_response(status);self.send_header('Content-Type',content_type);self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)
        def do_CONNECT(self):
            requests.append(dict(engine=self.server.engine,method='CONNECT',path=self.path,passed=False));self.send(403,b'Unexpected outbound connection','text/plain')
        def do_GET(self):
            parsed=urllib.parse.urlsplit(self.path);q=dict(urllib.parse.parse_qsl(parsed.query));valid=parsed.path=='/federation/credentials' and q.get('account_id')=='123456789012' and q.get('role_name')=='DatabaseAdministrator' and self.headers.get('x-amz-sso_bearer_token')=='fixture-sso-token'
            requests.append(dict(engine=self.server.engine,method='GET',path=self.path,kind='sso',valid=valid))
            if not valid:self.send(403,b'{"message":"Invalid fixture request"}','application/json');return
            self.send(200,json.dumps(dict(roleCredentials=dict(accessKeyId='fixture-access',secretAccessKey='fixture-secret',sessionToken='fixture-session',expiration=int(expiry.timestamp()*1000),accountId='123456789012'))).encode(),'application/json')
        def do_POST(self):
            body=self.rfile.read(int(self.headers.get('Content-Length','0'))).decode();q=dict(urllib.parse.parse_qsl(body));valid=q.get('Action')=='AssumeRoleWithWebIdentity' and q.get('WebIdentityToken')==TOKEN;accepted=valid and q.get('RoleArn')==ROLE
            requests.append(dict(engine=self.server.engine,method='POST',path=self.path,kind='sts',action=q.get('Action'),role=q.get('RoleArn'),valid=valid,accepted=accepted))
            if not accepted:self.send(403,b'<ErrorResponse><Error><Type>Sender</Type><Code>AccessDenied</Code><Message>Owned role rejected</Message></Error><RequestId>fixture</RequestId></ErrorResponse>','text/xml');return
            payload=f'<AssumeRoleWithWebIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><AssumeRoleWithWebIdentityResult><Credentials><AccessKeyId>fixture-access</AccessKeyId><SecretAccessKey>fixture-secret</SecretAccessKey><SessionToken>fixture-session</SessionToken><Expiration>{expiration}</Expiration></Credentials><SubjectFromWebIdentityToken>fixture</SubjectFromWebIdentityToken></AssumeRoleWithWebIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></AssumeRoleWithWebIdentityResponse>'
            self.send(200,payload.encode(),'text/xml')
    tls=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);plain=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);context=certificate(out,args.transport_negative);context.set_servername_callback(lambda sock,name,ctx: requests.append(dict(engine=getattr(tls,'engine',''),kind='tls',server_name=name)));tls.socket=context.wrap_socket(tls.socket,server_side=True)
    threads=[threading.Thread(target=s.serve_forever,daemon=True) for s in [tls,plain]]
    for t in threads:t.start()
    manifest=json.loads((ROOT/'extensions/aws/upstream.json').read_text());reference=out/'reference'
    for name in files:
        if args.sso_control:
            expected='ok' if args.sso_control=='valid' else 'error'
            suffix='' if expected=='ok' else '\n----\nSecret Validation Failure'
            data=("require aws\n\nrequire httpfs\n\nstatement "+expected+"\nCREATE SECRET control(TYPE s3,PROVIDER credential_chain,CHAIN 'sso',PROFILE 'DatabaseAdministrator-xxx')"+suffix+"\n\nquery I\nSELECT count(*) FROM duckdb_secrets()\n----\n"+('1' if expected=='ok' else '0')+'\n').encode()
        else:
            data=(args.source/name).read_bytes();assert hashlib.sha256(data).hexdigest()==manifest['sql_tests'][name]
        p=reference/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(data)
    profile=out/'profile';profile.mkdir();(profile/'credentials').write_text('')
    start_url='https://fixture.invalid/'+hashlib.sha256(str(out).encode()).hexdigest()[:16]
    (profile/'config').write_text('[default]\nregion=eu-west-1\n[profile DatabaseAdministrator-xxx]\nsso_start_url='+start_url+'\nsso_region=eu-west-1\nsso_account_id=123456789012\nsso_role_name=DatabaseAdministrator\nregion=eu-west-1\n')
    cache=profile/'sso/cache';cache.mkdir(parents=True);cache_file=cache/(hashlib.sha1(start_url.encode()).hexdigest()+'.json')
    cached=dict(accessToken='fixture-sso-token',expiresAt=expiration,region='eu-west-1',startUrl=start_url)
    if args.sso_control=='expired':cached['expiresAt']='2000-01-01T00:00:00Z'
    if args.sso_control=='invalid-expiry':cached['expiresAt']='not-a-date'
    if args.sso_control=='empty-token':cached['accessToken']=''
    if args.sso_control=='rejected-token':cached['accessToken']='rejected-token'
    if args.sso_control!='missing':cache_file.write_text('{invalid-json' if args.sso_control=='malformed' else json.dumps(cached))
    token=out/'web-token';token.write_text(TOKEN);fixed=Path('/tmp/duckdb-oidc-test-token');linked=False
    try:
        if not args.sso_control:fixed.symlink_to(token);linked=True
        for engine in (['cpp','rust'] if args.engine=='both' else [args.engine]):
            tls.engine=plain.engine=engine;directory=out/engine;directory.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir()
            platform=subprocess.check_output([str(args.oracle.resolve()),'-batch','-noheader','-csv','-init','/dev/null',':memory:','PRAGMA platform;'],text=True).strip()
            install=directory/'extensions'/'v1.5.6'/platform;install.mkdir(parents=True)
            for extension in args.extensions.glob('*.duckdb_extension'):(install/extension.name).symlink_to(extension.resolve())
            env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','DUCKDB_','SLT_','RDS_','PG','DYLD_')) and k.lower() not in ['https_proxy','http_proxy','all_proxy','no_proxy']}
            endpoint=f'http://127.0.0.1:{plain.server_port}'
            env.update(AWS_CONFIG_FILE=str(profile/'config'),AWS_SHARED_CREDENTIALS_FILE=str(profile/'credentials'),AWS_EC2_METADATA_DISABLED='true',AWS_DEFAULT_REGION='eu-west-1',AWS_REGION='eu-west-1',AWS_ROLE_ARN=ROLE,AWS_WEB_IDENTITY_TOKEN_FILE=str(token),AWS_ROLE_SESSION_NAME='fixture-session',RDS_TEST_SERVER_AVAILABLE='1',AWS_ENDPOINT_URL=endpoint,AWS_ENDPOINT_URL_STS=endpoint,AWS_ENDPOINT_URL_SSO=endpoint,AWS_MAX_ATTEMPTS='1',SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'),TMPDIR=str(directory))
            if engine=='cpp':
                for key in ['AWS_ENDPOINT_URL','AWS_ENDPOINT_URL_STS','AWS_ENDPOINT_URL_SSO']:env.pop(key,None)
                env.update(DYLD_INSERT_LIBRARIES=str(args.transport.resolve()),ROASTEDDUCK_FIXTURE_TLS_PORT=str(tls.server_port),ROASTEDDUCK_FIXTURE_CA_DER=str(out/'ca.der'),ROASTEDDUCK_FIXTURE_CA_PEM=str(out/'ca.pem'),SSL_CERT_FILE=str(out/'ca.pem'),AWS_CA_BUNDLE=str(out/'ca.pem'))
            rows=[];before=len(requests)
            for index,name in enumerate(files):
                result=directory/(str(index)+'.jsonl');command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--timeout-secs','40','--require','aws','--require','httpfs','--out',str(result),'--log-dir',str(directory/'logs'),name]
                if engine=='cpp':command+=['--shim',str(args.shim.resolve())]
                with (directory/(str(index)+'.log')).open('w') as log:run=subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=70)
                records=[json.loads(l) for l in result.read_text().splitlines()];rows.extend(records);print(engine,name,records,flush=True)
            report[engine]=dict(rows=rows,requests=requests[before:])
    finally:
        if linked and fixed.is_symlink() and fixed.readlink()==token:fixed.unlink()
        for s in [tls,plain]:s.shutdown();s.server_close()
        for t in threads:t.join()
        (out/'requests.json').write_text(json.dumps(requests,indent=2)+'\n')
    report['transport_negative']=args.transport_negative
    report['sso_control']=args.sso_control
    report['scope']='Owned STS/SSO protocol fixture; C++ per-process DNS/socket/CA isolation; no live AWS acceptance'
    report['sha256']={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['slt','shim','transport']}
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if args.transport_negative:
        passed=all(r['status']=='fail' and r.get('failure',{}).get('line')==43 for r in report['cpp']['rows']) and not any(r.get('kind') in ['sts','sso'] for r in requests) and any(r.get('kind')=='tls' for r in requests)
    else:
        passed=all(r['status']=='pass' for e in ['cpp','rust'] if e in report for r in report[e]['rows'])
        for engine in ['cpp','rust']:
            if engine not in report:continue
            sent=report[engine]['requests']
            if args.sso_control:
                calls=[r for r in sent if r.get('kind')=='sso']
                passed &= bool(calls) if args.sso_control in ['valid','rejected-token'] else not calls
                if args.sso_control=='valid':passed &= all(r['valid'] for r in calls)
                if args.sso_control=='rejected-token':passed &= all(not r['valid'] for r in calls)
                continue
            passed &= any(r.get('kind')=='sso' and r.get('valid') for r in sent) and sum(r.get('kind')=='sts' and r.get('accepted',False) for r in sent)>=2 and any(r.get('kind')=='sts' and r.get('valid') and not r.get('accepted') for r in sent)
    report['passed']=bool(passed)
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not passed:raise SystemExit(1)
if __name__=='__main__':main()
