#!/usr/bin/env python3
"""Paired session SSO refresh/cache controls on owned OIDC and SSO endpoints."""
import argparse
from datetime import datetime,timedelta,timezone
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import threading
import urllib.parse
import upstream_identity as identity

identity.HOSTS.extend(['oidc.eu-west-1.amazonaws.com','oidc.us-east-2.amazonaws.com'])
def literal(value):return "'"+str(value).replace("'","''")+"'"
def date(value):return value.strftime('%Y-%m-%dT%H:%M:%SZ')

CASES=['valid','outside-window','refresh-window','refresh','expired-refresh','registration-expired','refresh-failed-valid','refresh-failed-expired','missing-metadata','empty-response','expired-response','write-failed-valid','write-failed-expired','refresh-reuse','missing-token','empty-token','inline-conflict','inline-equal','inline-conflict-legacy','missing-session-legacy','legacy-no-refresh','default-chain','profile-default-env','profile-default-precedence','explicit-no-profile']

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','extensions','transport','out']:parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--case',action='append',choices=CASES);parser.add_argument('--engine',choices=['cpp','rust','both'],default='both')
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);requests=[];results=[]
    now=datetime.now(timezone.utc);future=now+timedelta(hours=2)
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def reply(self,status,body):
            payload=json.dumps(body).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)
        def do_POST(self):
            body=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))));valid=self.path=='/token' and body==self.server.oidc_body
            requests.append(dict(case=self.server.case,engine=self.server.engine,kind='oidc',host=self.headers.get('Host'),body=body,valid=valid))
            if not valid or self.server.case in ['refresh-failed-valid','refresh-failed-expired','missing-metadata']:
                self.reply(400,{'__type':'InvalidGrantException','error':'invalid_grant','error_description':'owned fixture denial'});return
            if self.server.case=='empty-response':self.reply(200,{});return
            self.reply(200,dict(accessToken='refreshed-token',expiresIn=-1 if self.server.case=='expired-response' else 3600,refreshToken='rotated-refresh',clientId='ignored-response-client',tokenType='Bearer'))
        def do_GET(self):
            parsed=urllib.parse.urlsplit(self.path);q=dict(urllib.parse.parse_qsl(parsed.query));token=self.headers.get('x-amz-sso_bearer_token')
            valid=parsed.path=='/federation/credentials' and q=={'account_id':'123456789012','role_name':'DatabaseAdministrator'} and token==self.server.expected_token
            requests.append(dict(case=self.server.case,engine=self.server.engine,kind='sso',token=token,valid=valid))
            if not valid:self.reply(403,{'__type':'UnauthorizedException','message':'owned fixture token rejected'});return
            self.reply(200,dict(roleCredentials=dict(accessKeyId='fixture-access',secretAccessKey='fixture-secret',sessionToken='fixture-session',expiration=int(future.timestamp()*1000),accountId='123456789012')))
    tls=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);plain=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);tls.socket=identity.certificate(out).wrap_socket(tls.socket,server_side=True)
    threads=[threading.Thread(target=s.serve_forever,daemon=True) for s in [tls,plain]]
    for t in threads:t.start()
    try:
        for case in args.case or CASES:
            runs={}
            for engine in ['cpp','rust'] if args.engine=='both' else [args.engine]:
                directory=out/(case+'-'+engine);directory.mkdir();(directory/'secrets').mkdir();profile=directory/'profile';profile.mkdir();(profile/'credentials').write_text('')
                near=case not in ['valid','outside-window','missing-token','empty-token','inline-conflict','inline-equal','inline-conflict-legacy','missing-session-legacy','legacy-no-refresh','default-chain','profile-default-env','profile-default-precedence','explicit-no-profile']
                old_expiry=now-timedelta(seconds=300) if case in ['expired-refresh','refresh-failed-expired','write-failed-expired'] else now+timedelta(seconds=300) if near else future
                if case=='refresh-window':old_expiry=now+timedelta(seconds=540)
                if case=='outside-window':old_expiry=now+timedelta(seconds=900)
                if case=='legacy-no-refresh':old_expiry=now+timedelta(seconds=300)
                cached=dict(accessToken='' if case=='empty-token' else 'fixture-sso-token',expiresAt=date(old_expiry),refreshToken='fixture-refresh',clientId='fixture-client',clientSecret='fixture-client-secret',registrationExpiresAt=date(now-timedelta(days=1) if case=='registration-expired' else future),region='us-east-2',startUrl='https://fixture.invalid/start',unknownField='drop-only-when-written')
                if case=='missing-metadata':
                    for key in ['refreshToken','clientId','clientSecret']:cached.pop(key)
                initial=json.dumps(cached,indent=2)+'\n';cache=profile/'sso/cache';cache.mkdir(parents=True);cache_file=cache/(hashlib.sha1(b'owned-session').hexdigest()+'.json')
                if case!='missing-token':cache_file.write_text(initial)
                if case.startswith('write-failed'):cache_file.chmod(0o400)
                selected='default' if case in ['default-chain','explicit-no-profile'] else 'profile selected'
                inline='sso_start_url=https://conflict.invalid\nsso_region=eu-west-1\n' if case in ['inline-conflict','inline-conflict-legacy'] else 'sso_start_url=https://fixture.invalid/start\nsso_region=eu-west-1\n' if case in ['inline-equal','missing-session-legacy','legacy-no-refresh'] else ''
                if case in ['inline-conflict-legacy','missing-session-legacy','legacy-no-refresh']:
                    url='https://conflict.invalid' if case=='inline-conflict-legacy' else 'https://fixture.invalid/start'
                    (cache/(hashlib.sha1(url.encode()).hexdigest()+'.json')).write_text(initial)
                (profile/'config').write_text('['+selected+']\nregion=eu-west-1\nsso_session=owned-session\nsso_account_id=123456789012\nsso_role_name=DatabaseAdministrator\n'+inline+'\n[sso-session owned-session]\nsso_start_url=https://fixture.invalid/start\nsso_region=eu-west-1\n')
                if case in ['missing-session-legacy','legacy-no-refresh']:
                    config=(profile/'config').read_text().split('[sso-session')[0]
                    if case=='legacy-no-refresh':config=config.replace('sso_session=owned-session\n','')
                    (profile/'config').write_text(config)
                refresh=near;success=case not in ['refresh-failed-expired','expired-response','write-failed-expired','missing-token','empty-token','inline-conflict','explicit-no-profile']
                new_token=case in ['refresh-window','refresh','expired-refresh','registration-expired','refresh-reuse']
                for server in [tls,plain]:
                    server.case=case;server.engine=engine;server.expected_token='refreshed-token' if new_token else 'fixture-sso-token';server.oidc_body={k:v for k,v in [('clientId',cached.get('clientId')),('clientSecret',cached.get('clientSecret')),('refreshToken',cached.get('refreshToken')),('grantType','refresh_token')] if v}
                env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','DUCKDB_','RDS_','PG','DYLD_')) and k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
                endpoint=f'http://127.0.0.1:{plain.server_port}'
                env.update(AWS_CONFIG_FILE=str(profile/'config'),AWS_SHARED_CREDENTIALS_FILE=str(profile/'credentials'),AWS_EC2_METADATA_DISABLED='true',AWS_REGION='eu-west-1',AWS_DEFAULT_REGION='eu-west-1',AWS_ENDPOINT_URL=endpoint,AWS_MAX_ATTEMPTS='1',TMPDIR=str(directory))
                if case in ['profile-default-env','profile-default-precedence']:
                    env['AWS_DEFAULT_PROFILE']='selected'
                    if case=='profile-default-precedence':
                        env['AWS_PROFILE']='other'
                        with (profile/'config').open('a') as config:config.write('\n[profile other]\nregion=eu-west-1\n')
                if engine=='cpp':
                    env.pop('AWS_ENDPOINT_URL');env.update(DYLD_INSERT_LIBRARIES=str(args.transport.resolve()),ROASTEDDUCK_FIXTURE_TLS_PORT=str(tls.server_port),ROASTEDDUCK_FIXTURE_CA_DER=str(out/'ca.der'),ROASTEDDUCK_FIXTURE_CA_PEM=str(out/'ca.pem'),SSL_CERT_FILE=str(out/'ca.pem'))
                (directory/'extensions').mkdir()
                env.update(SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'),SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'))
                options='' if case in ['default-chain','profile-default-env','profile-default-precedence'] else ",CHAIN 'sso'"+("" if case=='explicit-no-profile' else ",PROFILE 'selected'")
                sql="CREATE SECRET a(TYPE s3,PROVIDER credential_chain"+options+")"
                test="require aws\n\nrequire httpfs\n\nstatement "+('ok' if success else 'error')+"\n"+sql+('' if success else '\n----\nSecret Validation Failure')+'\n\n'
                if case=='refresh-reuse':test+="statement ok\nCREATE SECRET b(TYPE s3,PROVIDER credential_chain"+options+")\n\n"
                test+="query I\nSELECT count(*) FROM duckdb_secrets()\n----\n"+('2' if case=='refresh-reuse' else '1' if success else '0')+'\n'
                reference=directory/'reference';reference.mkdir();(reference/'control.test').write_text(test)
                before=len(requests);command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--timeout-secs','40','--out',str(directory/'result.jsonl'),'--log-dir',str(directory/'logs'),'control.test']
                if engine=='cpp':command+=['--shim',str(args.shim.resolve())]
                with (directory/'run.log').open('w') as log:run=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT,env=env,timeout=50)
                rows=[json.loads(l) for l in (directory/'result.jsonl').read_text().splitlines()]
                sent=requests[before:];after=cache_file.read_text() if cache_file.exists() else None
                (directory/'cache-before.json').write_text(initial);(directory/'cache-after.json').write_text(after or '')
                passed=len(rows)==1 and rows[0]['status']=='pass'
                passed &= all(r['valid'] for r in sent)
                oidc=[r for r in sent if r['kind']=='oidc'];sso=[r for r in sent if r['kind']=='sso']
                passed &= len(oidc)==int(refresh) and len(sso)==(2 if case=='refresh-reuse' else int(success))
                if engine=='cpp' and oidc:passed &= all(r['host']=='oidc.us-east-2.amazonaws.com' for r in oidc)
                if refresh and not case.startswith('write-failed'):
                    stored=json.loads(after);passed &= 'unknownField' not in stored
                    if new_token:
                        passed &= stored.get('accessToken')=='refreshed-token' and stored.get('refreshToken')=='rotated-refresh' and stored.get('clientId')=='fixture-client'
                        expiry=datetime.fromisoformat(stored['expiresAt'].replace('Z','+00:00'));passed &= abs((expiry-datetime.now(timezone.utc)).total_seconds()-3600)<10 and '.' not in stored['expiresAt']
                elif case!='missing-token':passed &= after==initial
                record=dict(case=case,engine=engine,exit=run.returncode,rows=rows,requests=sent,passed=bool(passed));runs[engine]=record;results.append(record);print(json.dumps(record),flush=True)
    finally:
        for server in [tls,plain]:server.shutdown();server.server_close()
        for t in threads:t.join()
    report=dict(cases=results,scope='Owned session SSO/OIDC fixture; no live AWS acceptance',sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['slt','shim','transport']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
