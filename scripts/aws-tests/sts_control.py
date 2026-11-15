#!/usr/bin/env python3
"""Paired signed AssumeRole controls with independent SigV4 verification."""
import argparse
from datetime import datetime,timedelta,timezone
import hashlib
import hmac
import http.server
import json
import os
from pathlib import Path
import re
import subprocess
import threading
import urllib.parse
import upstream_identity as identity

ROLE='arn:aws:iam::123456789012:role/owned-role'
KEYS={'fixture-env':('env-secret','env-session'),'fixture-default':('default-secret',''),'fixture-alternate':('alternate-secret','alternate-session'),'fixture-named':('named-secret','named-session')}
CASES=['basic','sql-region','env-region-precedence','external-id','config-role','config-override','default-profile-base','source-profile-ignored','sql-session-ignored','denied','malformed','expired','missing-token','missing-expiry','empty-key']

def verify(handler,body):
    try:
        algorithm,parts=handler.headers['Authorization'].split(' ',1);fields=dict(s.strip().split('=',1) for s in parts.split(','));access,day,region,service,end=fields['Credential'].split('/')
        secret,token=KEYS[access];assert algorithm=='AWS4-HMAC-SHA256' and service=='sts' and end=='aws4_request'
        assert handler.headers.get('x-amz-security-token','')==token
        stamp=handler.headers['x-amz-date'];assert stamp.startswith(day)
        parsed=urllib.parse.urlsplit(handler.path);encode=lambda x:urllib.parse.quote(x,safe='-_.~')
        query='&'.join(k+'='+v for k,v in sorted((encode(k),encode(v)) for k,v in urllib.parse.parse_qsl(parsed.query,keep_blank_values=True)))
        uri=urllib.parse.quote(urllib.parse.unquote(parsed.path or '/'),safe='/-_.~');signed=fields['SignedHeaders']
        headers=''.join(name+':'+ ' '.join(handler.headers[name].split())+'\n' for name in signed.split(';'))
        payload_hash=hashlib.sha256(body).hexdigest();assert handler.headers.get('x-amz-content-sha256',payload_hash)==payload_hash
        canonical='\n'.join([handler.command,uri,query,headers,signed,payload_hash])
        to_sign='\n'.join([algorithm,stamp,'/'.join([day,region,service,end]),hashlib.sha256(canonical.encode()).hexdigest()])
        key=('AWS4'+secret).encode()
        for part in [day,region,service,end]:key=hmac.new(key,part.encode(),hashlib.sha256).digest()
        assert hmac.compare_digest(hmac.new(key,to_sign.encode(),hashlib.sha256).hexdigest(),fields['Signature'])
        return True,access,region
    except (ValueError,KeyError,AssertionError,AttributeError,TypeError):return False,None,None

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','extensions','transport','out']:parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--case',action='append',choices=CASES);parser.add_argument('--engine',choices=['cpp','rust','both'],default='both')
    parser.add_argument('--bad-ca',action='store_true');parser.add_argument('--bad-host',action='store_true')
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);requests=[];results=[]
    expiration=(datetime.now(timezone.utc)+timedelta(hours=2)).strftime('%Y-%m-%dT%H:%M:%SZ')
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def reply(self,status,body):
            payload=body.encode();self.send_response(status);self.send_header('Content-Type','text/xml');self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)
        def do_POST(self):
            body=self.rfile.read(int(self.headers.get('Content-Length','0')));fields=dict(urllib.parse.parse_qsl(body.decode(),keep_blank_values=True));valid,access,region=verify(self,body)
            contract=valid and access==self.server.expected_access and region=='eu-west-1' and fields.get('Action')=='AssumeRole' and fields.get('RoleArn')==ROLE and fields.get('DurationSeconds')=='900' and bool(re.fullmatch(r'aws-sdk-cpp-\d+',fields.get('RoleSessionName',''))) and fields.get('ExternalId','')==self.server.expected_external
            requests.append(dict(case=self.server.case,engine=self.server.engine,fields=fields,access=access,region=region,signature_valid=valid,contract_valid=bool(contract)))
            if not valid or self.server.case=='denied':self.reply(403,'<ErrorResponse><Error><Type>Sender</Type><Code>AccessDenied</Code><Message>Owned role rejected</Message></Error><RequestId>fixture</RequestId></ErrorResponse>');return
            if self.server.case=='malformed':self.reply(200,'<invalid');return
            key='' if self.server.case=='empty-key' else 'assumed-access'
            token='' if self.server.case=='missing-token' else '<SessionToken>assumed-token</SessionToken>'
            expiry='' if self.server.case=='missing-expiry' else '<Expiration>'+('2000-01-01T00:00:00Z' if self.server.case=='expired' else expiration)+'</Expiration>'
            self.reply(200,'<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><AssumeRoleResult><Credentials><AccessKeyId>'+key+'</AccessKeyId><SecretAccessKey>assumed-secret</SecretAccessKey>'+token+expiry+'</Credentials><AssumedRoleUser><Arn>arn:aws:sts::123456789012:assumed-role/owned-role/fixture</Arn><AssumedRoleId>fixture:role</AssumedRoleId></AssumedRoleUser></AssumeRoleResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></AssumeRoleResponse>')
    tls=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);plain=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);tls.socket=identity.certificate(out,'ca' if args.bad_ca else 'hostname' if args.bad_host else None).wrap_socket(tls.socket,server_side=True)
    threads=[threading.Thread(target=s.serve_forever,daemon=True) for s in [tls,plain]]
    for thread in threads:thread.start()
    try:
        for case in args.case or CASES:
            for engine in ['cpp','rust'] if args.engine=='both' else [args.engine]:
                directory=out/(case+'-'+engine);directory.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir()
                (directory/'credentials').write_text('[default]\naws_access_key_id=fixture-default\naws_secret_access_key=default-secret\n[alternate]\naws_access_key_id=fixture-alternate\naws_secret_access_key=alternate-secret\naws_session_token=alternate-session\n[roleprofile]\naws_access_key_id=fixture-named\naws_secret_access_key=named-secret\naws_session_token=named-session\n')
                (directory/'config').write_text('[default]\nregion=eu-west-1\n[profile alternate]\nregion=eu-west-1\n[profile roleprofile]\nregion=us-west-2\nrole_arn='+ROLE+'\nexternal_id=profile-external\nsource_profile=alternate\nrole_session_name=config-ignored\n[profile bare]\nregion=eu-west-1\n')
                env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','DUCKDB_','SLT_','PG','RDS_','DYLD_')) and k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
                env.update(AWS_CONFIG_FILE=str(directory/'config'),AWS_SHARED_CREDENTIALS_FILE=str(directory/'credentials'),AWS_EC2_METADATA_DISABLED='true',AWS_ACCESS_KEY_ID='fixture-env',AWS_SECRET_ACCESS_KEY='env-secret',AWS_SESSION_TOKEN='env-session',AWS_DEFAULT_REGION='eu-west-1',AWS_REGION='us-east-1' if case=='env-region-precedence' else 'eu-west-1',AWS_MAX_ATTEMPTS='1',AWS_ENDPOINT_URL=f'http://127.0.0.1:{plain.server_port}',SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'),TMPDIR=str(directory))
                if case=='default-profile-base':
                    for key in ['AWS_ACCESS_KEY_ID','AWS_SECRET_ACCESS_KEY','AWS_SESSION_TOKEN']:env.pop(key)
                    env.update(AWS_DEFAULT_PROFILE='alternate',AWS_PROFILE='default')
                env.pop('AWS_ENDPOINT_URL');env.update(DYLD_INSERT_LIBRARIES=str(args.transport.resolve()),ROASTEDDUCK_FIXTURE_TLS_PORT=str(tls.server_port),ROASTEDDUCK_FIXTURE_CA_DER=str(out/'ca.der'),ROASTEDDUCK_FIXTURE_CA_PEM=str(out/'ca.pem'),SSL_CERT_FILE=str(out/'ca.pem'))
                external='profile-external' if case in ['config-role','source-profile-ignored'] else 'fixture-id/with:parts' if case=='external-id' else ''
                for server in [tls,plain]:server.case=case;server.engine=engine;server.expected_access='fixture-alternate' if case=='default-profile-base' else 'fixture-env';server.expected_external=external
                options=",CHAIN 'config',PROFILE 'roleprofile'" if case in ['config-role','source-profile-ignored'] else ",CHAIN 'config',PROFILE 'bare',ASSUME_ROLE_ARN '"+ROLE+"'" if case=='config-override' else ",CHAIN 'sts',ASSUME_ROLE_ARN '"+ROLE+"'"
                if case=='external-id':options+=",EXTERNAL_ID '"+external+"'"
                if case=='sql-region':options+=",REGION 'us-west-2'"
                if case=='sql-session-ignored':options+=",SESSION_NAME 'should-be-ignored'"
                success=case not in ['denied','malformed','empty-key'] and not args.bad_ca and not args.bad_host
                test="require aws\n\nrequire httpfs\n\nstatement "+('ok' if success else 'error')+"\nCREATE SECRET a(TYPE s3,PROVIDER credential_chain"+options+")"+('' if success else '\n----\nSecret Validation Failure')+'\n\n'
                test+='query I\nSELECT count(*) FROM duckdb_secrets()\n----\n'+('1' if success else '0')+'\n'
                if success:
                    test+="\nquery I\nSELECT count(*) FROM duckdb_secrets(redact=false) WHERE secret_string LIKE '%key_id=assumed-access%' AND secret_string LIKE '%secret=assumed-secret%'\n----\n1\n"
                    if case in ['sql-region','env-region-precedence']:test+="\nquery I\nSELECT count(*) FROM duckdb_secrets() WHERE secret_string LIKE '%region="+('us-west-2' if case=='sql-region' else 'us-east-1')+"%'\n----\n1\n"
                reference=directory/'reference';reference.mkdir();(reference/'control.test').write_text(test)
                before=len(requests);command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--timeout-secs','40','--out',str(directory/'result.jsonl'),'--log-dir',str(directory/'logs'),'control.test']
                if engine=='cpp':command+=['--shim',str(args.shim.resolve())]
                with (directory/'run.log').open('w') as log:subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=50)
                rows=[json.loads(l) for l in (directory/'result.jsonl').read_text().splitlines()];sent=requests[before:]
                record=dict(case=case,engine=engine,rows=rows,requests=sent,passed=len(rows)==1 and rows[0]['status']=='pass' and (not sent if args.bad_ca or args.bad_host else bool(sent)) and all(r['contract_valid'] for r in sent));results.append(record);print(json.dumps(record),flush=True)
    finally:
        for server in [tls,plain]:server.shutdown();server.server_close()
        for thread in threads:thread.join()
    (out/'verification.json').write_text(json.dumps(dict(cases=results,bad_ca=args.bad_ca,bad_host=args.bad_host,sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['slt','shim','transport']},scope='Owned STS fixture and independent signatures; not live IAM acceptance'),indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
