#!/usr/bin/env python3
"""Paired web-identity profile, override and TLS routing controls."""
import argparse
from datetime import datetime,timedelta,timezone
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import subprocess
import threading
import urllib.parse
import upstream_identity as identity

ROLES={name:'arn:aws:iam::123456789012:role/'+name for name in ['default','named','other','override']}
CASES=['missing-region','default-missing-region','sql-region-only','profile-only','role-override','token-override','session-override','env-overrides','sql-profile-ignored','env-profile','default-profile-explicit','profile-precedence-explicit','default-profile-chain','profile-precedence-chain','default-env-session','region-explicit','region-default','region-default-profile','secret-region','missing-file','empty-env-fallback']
def literal(value):return "'"+str(value).replace("'","''")+"'"

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','extensions','transport','out']:parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--case',action='append',choices=CASES);parser.add_argument('--engine',choices=['cpp','rust','both'],default='both')
    parser.add_argument('--bad-ca',action='store_true');parser.add_argument('--bad-host',action='store_true')
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);requests=[];results=[]
    expiry=(datetime.now(timezone.utc)+timedelta(hours=2)).strftime('%Y-%m-%dT%H:%M:%SZ')
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_POST(self):
            body=self.rfile.read(int(self.headers.get('Content-Length','0'))).decode();q=dict(urllib.parse.parse_qsl(body,keep_blank_values=True));expected=self.server.expected
            host=self.headers.get('Host');region=host.removeprefix('sts.').removesuffix('.amazonaws.com')
            name=q.get('RoleSessionName','');session_ok=bool(re.fullmatch(r'[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}',name)) if expected['session']=='uuid' else name==expected['session']
            valid=q.get('Action')=='AssumeRoleWithWebIdentity' and q.get('RoleArn')==ROLES[expected['role']] and q.get('WebIdentityToken')==expected['token'] and region==expected['region'] and session_ok
            requests.append(dict(case=self.server.case,engine=self.server.engine,fields=q,region=region,valid=bool(valid)))
            payload=('<AssumeRoleWithWebIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/"><AssumeRoleWithWebIdentityResult><Credentials><AccessKeyId>web-access</AccessKeyId><SecretAccessKey>web-secret</SecretAccessKey><SessionToken>web-session</SessionToken><Expiration>'+expiry+'</Expiration></Credentials><SubjectFromWebIdentityToken>fixture</SubjectFromWebIdentityToken></AssumeRoleWithWebIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></AssumeRoleWithWebIdentityResponse>').encode()
            self.send_response(200);self.send_header('Content-Type','text/xml');self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.socket=identity.certificate(out,'ca' if args.bad_ca else 'hostname' if args.bad_host else None).wrap_socket(server.socket,server_side=True);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        for case in args.case or CASES:
            for engine in ['cpp','rust'] if args.engine=='both' else [args.engine]:
                directory=out/(case+'-'+engine);directory.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir();(directory/'credentials').write_text('')
                for name in ['default','named','other','env']:(directory/(name+'-token')).write_text(name+'-token')
                config=''
                for name in ['default','named','other']:
                    config+='['+('default' if name=='default' else 'profile '+name)+']\nregion=eu-west-1\nrole_arn='+ROLES[name]+'\nweb_identity_token_file='+str(directory/(name+'-token'))+'\nrole_session_name='+name+'-session\n'
                (directory/'config').write_text(config)
                env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','DUCKDB_','SLT_','PG','RDS_','DYLD_')) and k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
                env.update(AWS_CONFIG_FILE=str(directory/'config'),AWS_SHARED_CREDENTIALS_FILE=str(directory/'credentials'),AWS_EC2_METADATA_DISABLED='true',AWS_DEFAULT_REGION='eu-west-1',AWS_REGION='eu-west-1',AWS_MAX_ATTEMPTS='1',DYLD_INSERT_LIBRARIES=str(args.transport.resolve()),ROASTEDDUCK_FIXTURE_TLS_PORT=str(server.server_port),ROASTEDDUCK_FIXTURE_CA_DER=str(out/'ca.der'),ROASTEDDUCK_FIXTURE_CA_PEM=str(out/'ca.pem'),SSL_CERT_FILE=str(out/'ca.pem'),SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'),TMPDIR=str(directory))
                default_chain=case in ['default-missing-region','default-profile-chain','profile-precedence-chain','default-env-session','region-default','region-default-profile']
                options='' if default_chain else ",CHAIN 'web_identity'"
                expected=dict(role='default',token='default-token',region='eu-west-1',session='default-session' if default_chain else 'uuid')
                if case=='role-override':options+=",ASSUME_ROLE_ARN "+literal(ROLES['override']);expected['role']='override'
                if case=='token-override':options+=",WEB_IDENTITY_TOKEN_FILE "+literal(directory/'env-token');expected['token']='env-token'
                if case=='session-override':options+=",SESSION_NAME 'sql-session'";expected['session']='sql-session'
                if case in ['env-overrides','default-env-session']:
                    env.update(AWS_ROLE_ARN=ROLES['other'],AWS_WEB_IDENTITY_TOKEN_FILE=str(directory/'env-token'),AWS_ROLE_SESSION_NAME='env-session');expected.update(role='other',token='env-token',session='env-session' if default_chain else 'uuid')
                if case=='sql-profile-ignored':options+=",PROFILE 'named'"
                if case=='env-profile':env['AWS_PROFILE']='named';expected.update(role='named',token='named-token')
                if case in ['default-profile-explicit','default-profile-chain']:
                    env['AWS_DEFAULT_PROFILE']='named'
                    if default_chain:expected.update(role='named',token='named-token',session='named-session')
                if case in ['profile-precedence-explicit','profile-precedence-chain']:
                    env.update(AWS_DEFAULT_PROFILE='named',AWS_PROFILE='other');selected='named' if default_chain else 'other';expected.update(role=selected,token=selected+'-token',session=selected+'-session' if default_chain else 'uuid')
                if case in ['region-explicit','region-default','region-default-profile']:
                    env['AWS_REGION']='us-east-1'
                    if case=='region-explicit':expected['region']='us-east-1'
                    if case=='region-default-profile':env.pop('AWS_DEFAULT_REGION')
                if case=='secret-region':options+=",REGION 'us-west-2'"
                if case=='missing-file':options+=",WEB_IDENTITY_TOKEN_FILE "+literal(directory/'missing')
                if case=='empty-env-fallback':env.update(AWS_ROLE_ARN='',AWS_WEB_IDENTITY_TOKEN_FILE='',AWS_ROLE_SESSION_NAME='')
                if case in ['missing-region','default-missing-region','sql-region-only']:
                    env.pop('AWS_REGION');env.pop('AWS_DEFAULT_REGION')
                    (directory/'config').write_text(config.replace('region=eu-west-1\n',''))
                    if case=='sql-region-only':options+=",REGION 'us-east-1'"
                server.case=case;server.engine=engine;server.expected=expected
                success=case not in ['missing-file','missing-region','default-missing-region','sql-region-only'] and not args.bad_ca and not args.bad_host
                test="require aws\n\nrequire httpfs\n\nstatement "+('ok' if success else 'error')+"\nCREATE SECRET a(TYPE s3,PROVIDER credential_chain"+options+")"+('' if success else '\n----\nSecret Validation Failure')+'\n\n'
                test+="query I\nSELECT count(*) FROM duckdb_secrets()\n----\n"+('1' if success else '0')+'\n'
                if success:test+="\nquery I\nSELECT count(*) FROM duckdb_secrets(redact=false) WHERE secret_string LIKE '%key_id=web-access%' AND secret_string LIKE '%secret=web-secret%'\n----\n1\n"
                reference=directory/'reference';reference.mkdir();(reference/'control.test').write_text(test)
                before=len(requests);cmd=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--timeout-secs','35','--out',str(directory/'result.jsonl'),'--log-dir',str(directory/'logs'),'control.test']
                if engine=='cpp':cmd+=['--shim',str(args.shim.resolve())]
                with (directory/'run.log').open('w') as log:subprocess.run(cmd,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=45)
                rows=[json.loads(l) for l in (directory/'result.jsonl').read_text().splitlines()];sent=requests[before:]
                record=dict(case=case,engine=engine,rows=rows,requests=sent,passed=len(rows)==1 and rows[0]['status']=='pass' and (bool(sent) if success else not sent) and all(r['valid'] for r in sent));results.append(record);print(json.dumps(record),flush=True)
    finally:server.shutdown();server.server_close();thread.join()
    (out/'verification.json').write_text(json.dumps(dict(cases=results,bad_ca=args.bad_ca,bad_host=args.bad_host,scope='Owned TLS STS endpoint; unmodified provider wire traffic and profile inputs',sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['slt','shim','transport']}),indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
