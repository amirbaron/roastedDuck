#!/usr/bin/env python3
"""Paired ECS/IMDS credentials controls against an owned loopback HTTP service."""
import argparse
from datetime import datetime,timedelta,timezone
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import threading
import time

CASES=['ecs-retry-once','ecs-retry-exhaust','ecs-timeout','ecs-empty-key','ecs-empty-file', 'ecs-empty-relative-public', 'ecs-denied-imds-enabled', 'imds-expired', 'imds-missing-expiry', 'imds-invalid-expiry', 'imds-internal-error', 'imds-no-session-token', 'ecs-relative', 'ecs-relative-wins', 'ecs-invalid-relative', 'ecs-empty-relative', 'ecs-anonymous', 'ecs-env-token', 'ecs-file-token', 'ecs-missing-file', 'ecs-crlf-file', 'ecs-crlf-env', 'ecs-invalid-json', 'ecs-denied', 'ecs-missing-key', 'ecs-expired', 'ecs-missing-expiry', 'ecs-invalid-expiry', 'ecs-no-session-token', 'imds-v2', 'imds-explicit', 'imds-disabled', 'imds-explicit-disabled', 'imds-v1', 'imds-v1-disabled', 'imds-bad-token', 'imds-empty-token', 'imds-invalid-json']

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','extensions','transport','out']:parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--case',action='append',choices=CASES);parser.add_argument('--engine',choices=['cpp','rust','both'],default='both')
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);requests=[];results=[]
    expiry=(datetime.now(timezone.utc)+timedelta(hours=2)).strftime('%Y-%m-%dT%H:%M:%SZ')
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def reply(self,status,body):
            body=body.encode() if isinstance(body,str) else body;self.send_response(status);self.send_header('Content-Length',str(len(body)));
            if self.command=='PUT' and status==200:self.send_header('x-aws-ec2-metadata-token-ttl-seconds','21600')
            self.end_headers()
            try:self.wfile.write(body)
            except (BrokenPipeError,ConnectionResetError):pass
        def do_PUT(self):
            valid=self.path=='/latest/api/token' and self.headers.get('x-aws-ec2-metadata-token-ttl-seconds')=='21600';requests.append(dict(case=self.server.case,engine=self.server.engine,method='PUT',path=self.path,valid=valid))
            if self.server.case in ['imds-v1','imds-v1-disabled']:self.reply(403,'');return
            if self.server.case=='imds-bad-token':self.reply(400,'');return
            self.reply(200,'' if self.server.case=='imds-empty-token' else 'fixture-imds-token')
        def do_GET(self):
            case=self.server.case;auth=self.headers.get('Authorization','');token=self.headers.get('x-aws-ec2-metadata-token','')
            if self.path=='/ecs/creds':valid=auth==self.server.expected_auth
            else:valid=token=='fixture-imds-token' or (not token and case in ['imds-v1','imds-empty-token'])
            requests.append(dict(case=case,engine=self.server.engine,method='GET',path=self.path,auth=auth,token=token,valid=valid))
            if not valid:self.reply(401,'');return
            if self.path.rstrip('/')=='/latest/meta-data/iam/security-credentials':self.reply(200,'fixture-role\n');return
            if self.path not in ['/ecs/creds','/latest/meta-data/iam/security-credentials/fixture-role']:self.reply(404,'');return
            if self.path=='/ecs/creds':
                self.server.calls+=1
                if case=='ecs-timeout':time.sleep(3)
                if (case=='ecs-retry-once' and self.server.calls==1) or (case=='ecs-retry-exhaust' and self.server.calls<=2):self.reply(503,'');return
            if case in ['ecs-denied','ecs-denied-imds-enabled']:self.reply(403,'');return
            if case in ['ecs-invalid-json','imds-invalid-json']:self.reply(200,'not-json');return
            payload=dict(Code='Success',AccessKeyId='fixture-access',SecretAccessKey='fixture-secret',Token='fixture-session',Expiration='2000-01-01T00:00:00Z' if case in ['ecs-expired','imds-expired'] else expiry,AccountId='123456789012')
            if case=='ecs-missing-key':payload.pop('AccessKeyId')
            if case=='ecs-empty-key':payload['AccessKeyId']=''
            if case in ['ecs-missing-expiry','imds-missing-expiry']:payload.pop('Expiration')
            if case in ['ecs-invalid-expiry','imds-invalid-expiry']:payload['Expiration']='bad-date'
            if case in ['ecs-no-session-token','imds-no-session-token']:payload.pop('Token')
            if case=='imds-internal-error':payload['Code']='InternalServiceException'
            self.reply(200,json.dumps(payload))
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        for case in args.case or CASES:
            for engine in ['cpp','rust'] if args.engine=='both' else [args.engine]:
                server.case=case;server.engine=engine;server.calls=0;server.expected_auth='fixture-file' if case=='ecs-file-token' else 'fixture-env' if case in ['ecs-env-token','ecs-empty-file'] else ''
                directory=out/(case+'-'+engine);directory.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir();(directory/'config').write_text('[default]\nregion=eu-west-1\n');(directory/'credentials').write_text('')
                env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','DUCKDB_','SLT_','RDS_','PG','DYLD_')) and k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
                endpoint=f'http://127.0.0.1:{server.server_port}'
                env.update(AWS_CONFIG_FILE=str(directory/'config'),AWS_SHARED_CREDENTIALS_FILE=str(directory/'credentials'),AWS_REGION='eu-west-1',AWS_DEFAULT_REGION='eu-west-1',AWS_EC2_METADATA_SERVICE_ENDPOINT=endpoint,AWS_METADATA_SERVICE_TIMEOUT='1',AWS_METADATA_SERVICE_NUM_ATTEMPTS='1',AWS_MAX_ATTEMPTS='1',TMPDIR=str(directory),SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'))
                if engine=='cpp':env.update(DYLD_INSERT_LIBRARIES=str(args.transport.resolve()))
                if case in ['ecs-relative','ecs-relative-wins','ecs-invalid-relative','ecs-empty-relative']:
                    env.update(DYLD_INSERT_LIBRARIES=str(args.transport.resolve()),ROASTEDDUCK_FIXTURE_HTTP_PORT=str(server.server_port))
                if case.startswith('ecs-'):
                    env.update(AWS_CONTAINER_CREDENTIALS_FULL_URI=endpoint+'/ecs/creds',AWS_EC2_METADATA_DISABLED='true')
                    if case in ['ecs-relative','ecs-relative-wins','ecs-invalid-relative','ecs-empty-relative']:
                        env['AWS_CONTAINER_CREDENTIALS_RELATIVE_URI']='bad-relative' if case=='ecs-invalid-relative' else '' if case=='ecs-empty-relative' else '/ecs/creds'
                        if case=='ecs-relative':env.pop('AWS_CONTAINER_CREDENTIALS_FULL_URI')
                        if case=='ecs-relative-wins':env['AWS_CONTAINER_CREDENTIALS_FULL_URI']=endpoint+'/wrong'
                    if case in ['ecs-env-token','ecs-file-token','ecs-missing-file','ecs-crlf-file']:env['AWS_CONTAINER_AUTHORIZATION_TOKEN']='fixture-env'
                    if case=='ecs-crlf-env':env['AWS_CONTAINER_AUTHORIZATION_TOKEN']='bad\r\ntoken'
                    if case in ['ecs-file-token','ecs-missing-file','ecs-crlf-file']:
                        env['AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE']=str(directory/'auth-token')
                        if case!='ecs-missing-file':(directory/'auth-token').write_text('fixture-file' if case=='ecs-file-token' else 'bad\r\ntoken')
                    if case=='ecs-empty-file':env.update(AWS_CONTAINER_AUTHORIZATION_TOKEN='fixture-env',AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE='')
                    if case=='ecs-empty-relative-public':env.update(AWS_CONTAINER_CREDENTIALS_RELATIVE_URI='',AWS_CONTAINER_CREDENTIALS_FULL_URI='http://192.0.2.1/ecs/creds')
                    if case=='ecs-denied-imds-enabled':env['AWS_EC2_METADATA_DISABLED']='false'
                else:
                    env['AWS_EC2_METADATA_DISABLED']='true' if case in ['imds-disabled','imds-explicit-disabled'] else 'false'
                    if case=='imds-v1-disabled':env['AWS_EC2_METADATA_V1_DISABLED']='true'
                success=case not in ['ecs-retry-exhaust','ecs-timeout','ecs-empty-key','ecs-denied-imds-enabled','ecs-empty-relative-public','ecs-invalid-relative','ecs-missing-file','ecs-crlf-file','ecs-crlf-env','ecs-invalid-json','ecs-denied','ecs-missing-key','imds-disabled','imds-v1-disabled','imds-bad-token','imds-invalid-json','imds-internal-error']
                chain=",CHAIN 'instance'" if case in ['imds-explicit','imds-explicit-disabled'] else ''
                sql="CREATE SECRET a(TYPE s3,PROVIDER credential_chain"+chain+")"
                test="require aws\n\nrequire httpfs\n\nstatement "+('ok' if success else 'error')+'\n'+sql+('' if success else '\n----\nSecret Validation Failure')+'\n\n'
                test+='query I\nSELECT count(*) FROM duckdb_secrets()\n----\n'+('1' if success else '0')+'\n'
                if success and case not in ['ecs-expired','ecs-missing-expiry','ecs-invalid-expiry']:test+="\nquery I\nSELECT count(*) FROM duckdb_secrets(redact=false) WHERE secret_string LIKE '%key_id=fixture-access%' AND secret_string LIKE '%secret=fixture-secret%'\n----\n1\n"
                if case in ['ecs-expired','ecs-missing-expiry','ecs-invalid-expiry']:test+="\nquery I\nSELECT count(*) FROM duckdb_secrets(redact=false) WHERE secret_string LIKE '%key_id=%'\n----\n0\n"
                reference=directory/'reference';reference.mkdir();(reference/'control.test').write_text(test)
                before=len(requests);cmd=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--timeout-secs','25','--out',str(directory/'result.jsonl'),'--log-dir',str(directory/'logs'),'control.test']
                if engine=='cpp':cmd+=['--shim',str(args.shim.resolve())]
                with (directory/'run.log').open('w') as log:run=subprocess.run(cmd,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=35)
                rows=[json.loads(l) for l in (directory/'result.jsonl').read_text().splitlines()];sent=requests[before:]
                passed=len(rows)==1 and rows[0]['status']=='pass'
                if case!='imds-v1-disabled':passed &= all(r['valid'] for r in sent)
                if case in ['ecs-empty-relative-public','ecs-invalid-relative','ecs-missing-file','ecs-crlf-file','ecs-crlf-env','imds-disabled']:passed &= not sent
                else:passed &= bool(sent)
                if case in ['ecs-retry-once','ecs-retry-exhaust','ecs-timeout']:passed &= len(sent)==2
                if case in ['ecs-invalid-json','ecs-missing-key','ecs-empty-key']:passed &= len(sent)==1
                if case=='ecs-denied-imds-enabled':passed &= not any(r['method']=='PUT' for r in sent)
                record=dict(case=case,engine=engine,rows=rows,requests=sent,passed=bool(passed));results.append(record);print(json.dumps(record),flush=True)
    finally:server.shutdown();server.server_close();thread.join()
    (out/'verification.json').write_text(json.dumps(dict(cases=results,sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['slt','shim','transport']},scope='Owned ECS/IMDS HTTP fixture; not live AWS'),indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
