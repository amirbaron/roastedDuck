#!/usr/bin/env python3
"""Owned PostgreSQL OAUTHBEARER protocol fixture; no real issuer or credentials."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
from rds_control import Endpoint, I, H, recv, packet, message
from codec_control import literal

ISSUER='https://issuer.fixture'
DISCOVERY=ISSUER+'/.well-known/openid-configuration'
class OAuthEndpoint(Endpoint):
    def __init__(self, accepted, challenge=None, mode='oauth'):
        self.accepted=set(accepted);self.challenge=challenge;self.exchanges=[];self.mode=mode
    def authenticate(self,conn,startup):
        pairs=startup[4:].rstrip(b'\0').split(b'\0');params=dict(zip(pairs[::2],pairs[1::2]));assert params[b'user']==b'admin'
        if self.mode=='trust':
            self.exchanges.append(dict(kind='trust'));message(conn,b'R',I(0));return True
        if self.mode=='password':
            message(conn,b'R',I(3))
            try:
                kind,body=packet(conn);self.exchanges.append(dict(kind='password-received'))
            except EOFError:pass
            return False
        message(conn,b'R',I(10)+b'OAUTHBEARER\0\0')
        try:kind,body=packet(conn)
        except EOFError:return False
        assert kind==b'p'
        mechanism,payload=body.split(b'\0',1);assert mechanism==b'OAUTHBEARER';length=struct.unpack('!i',payload[:4])[0];data=payload[4:];assert length==len(data)
        if data==b'n,,\x01auth=\x01\x01':
            self.exchanges.append(dict(kind='discovery'))
            response=self.challenge if self.challenge is not None else json.dumps({'status':'invalid_token','openid-configuration':DISCOVERY,'scope':'fixture'}) .encode()
            message(conn,b'R',I(11)+response)
            try:
                kind,body=packet(conn);self.exchanges.append(dict(kind='dummy',valid=kind==b'p' and body==b'\x01'))
            except EOFError:pass
            return False
        assert data.startswith(b'n,,\x01auth=Bearer ') and data.endswith(b'\x01\x01')
        token=data[len(b'n,,\x01auth=Bearer '):-2].decode();self.exchanges.append(dict(kind='token',token=token));self.tokens.append(hashlib.sha256(token.encode()).hexdigest())
        if token not in self.accepted:
            message(conn,b'R',I(11)+json.dumps({'status':'invalid_token','openid-configuration':DISCOVERY}).encode())
            try:kind,body=packet(conn);assert kind==b'p' and body==b'\x01'
            except EOFError:return False
            message(conn,b'E',b'SFATAL\0C28000\0MOAuth bearer authentication failed\0\0');return False
        if self.mode=='final':
            message(conn,b'R',I(12));return False
        message(conn,b'R',I(0));return True
    def shape(self,sql):
        if sql.strip().upper().startswith('COPY ('):return [],[],'COPY 1'
        if 'CURRENT_USER' in sql.upper():return [('current_user',19)],['admin'],'SELECT 1'
        return super().shape(sql)
    def copy_query(self,conn,sql):
        if sql.strip().upper().startswith('COPY (') and 'CURRENT_USER' in sql.upper():
            message(conn,b'H',b'\x01'+H(1)+H(1))
            body=b'PGCOPY\n\xff\r\n\0'+I(0)+I(0)+H(1)+I(5)+b'admin'+struct.pack('!h',-1)
            message(conn,b'd',body);message(conn,b'c');message(conn,b'C',b'COPY 1\0');return True
        return False

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--slt',type=Path);parser.add_argument('--shim',type=Path);parser.add_argument('--source',type=Path)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);results=[]
    scenarios=[
        ('session-priority',DISCOVERY,"SET pg_oauth_token='fixture-setting';",{'fixture-setting'},None,None),
        ('environment-fallback',DISCOVERY,"SET pg_oauth_token='';",{'fixture-env'},None,None),
        ('discovery-reconnect',ISSUER,"SET pg_oauth_token='fixture-setting';",{'fixture-setting'},None,None),
        ('issuer-mismatch',ISSUER,"SET pg_oauth_token='fixture-setting';",set(),b'{"status":"invalid_token","openid-configuration":"https://wrong.fixture/.well-known/openid-configuration"}','incompatible with oauth_issuer'),
        ('duplicate-status',ISSUER,"SET pg_oauth_token='fixture-setting';",set(),b'{"status":"invalid_token","status":"invalid_token","openid-configuration":"https://issuer.fixture/.well-known/openid-configuration"}','is duplicated'),
        ('nonstring-scope',ISSUER,"SET pg_oauth_token='fixture-setting';",set(),b'{"status":"invalid_token","scope":true}','must be a string'),
        ('rejected-token',DISCOVERY,"SET pg_oauth_token='fixture-rejected';",{'fixture-env'},None,'OAuth bearer authentication failed'),
        ('renewed-session',DISCOVERY,"SET pg_oauth_token='fixture-setting';",{'fixture-setting','fixture-renewed'},None,None),
    ]
    extra_options={
        'required-oauth':' require_auth=oauth',
        'forbidden-oauth':' require_auth=!oauth',
        'required-password':' require_auth=password',
        'required-oauth-trust':' require_auth=oauth',
        'required-oauth-password':' require_auth=oauth password=fixture-password',
        'policy-duplicate':' require_auth=oauth,oauth',
        'policy-mixed':' require_auth=oauth,!password',
        'policy-unknown':' require_auth=oauth,unknown',
        'channel-binding':' channel_binding=require',
    }
    for label,error in [
        ('session-cleared',None),('required-oauth',None),
        ('forbidden-oauth','server requested OAUTHBEARER authentication'),
        ('required-password','server requested SASL authentication'),
        ('required-oauth-trust','server did not complete authentication'),
        ('required-oauth-password','server requested a cleartext password'),
        ('policy-duplicate','is specified more than once'),
        ('policy-mixed','cannot be mixed with non-negative methods'),
        ('policy-unknown','invalid require_auth value'),
        ('channel-binding','channel binding required, but SSL not in use'),
        ('missing-client-id','oauth_issuer and oauth_client_id are not both set'),
        ('unexpected-final','server sent unexpected additional OAuth data'),
    ]:
        scenarios.append((label,DISCOVERY,"SET pg_oauth_token='fixture-setting';",{'fixture-setting','fixture-env'},None,error))
    for label,uri,error in [
        ('insecure-discovery','http://issuer.fixture/.well-known/openid-configuration','must use HTTPS'),
        ('query-discovery',DISCOVERY+'?x=y','must not contain query or fragment'),
        ('misplaced-discovery',ISSUER+'/path/.well-known/openid-configuration/extra','uses an invalid format'),
    ]:
        scenarios.append((label,ISSUER,"SET pg_oauth_token='fixture-setting';",set(),json.dumps({'status':'invalid_token','openid-configuration':uri}).encode(),error))
    for label,issuer,setting,accepted,challenge,error in scenarios:
        for engine in ['cpp','rust']:
            directory=out/(label+'-'+engine);directory.mkdir();(directory/'secrets').mkdir();(directory/'pgpass').write_text('');(directory/'pgservice').write_text('');(directory/'pgpass').chmod(0o600)
            env={k:v for k,v in os.environ.items() if not k.startswith(('PG','AWS_','RDS_','DUCKDB_AWS_'))};env.update(PGOAUTHTOKEN='fixture-env',PGPASSFILE=str(directory/'pgpass'),PGSERVICEFILE=str(directory/'pgservice'),TMPDIR=str(directory))
            with OAuthEndpoint(accepted,challenge,{'required-oauth-trust':'trust','required-oauth-password':'password','unexpected-final':'final'}.get(label,'oauth')) as endpoint:
                dsn=f"host=127.0.0.1 port={endpoint.port} user=admin dbname=fixture sslmode=disable oauth_issuer={issuer} oauth_client_id=fixture-client"
                dsn+=extra_options.get(label,'')
                if label=='missing-client-id':dsn=dsn.replace(' oauth_client_id=fixture-client','')
                load='LOAD '+literal(str(args.extension.resolve()))+';' if engine=='cpp' else 'LOAD postgres;'
                sql='SET secret_directory='+literal(str(directory/'secrets'))+';'+load+setting+'ATTACH '+literal(dsn)+" AS o(TYPE POSTGRES);"
                if label=='renewed-session':sql+="SELECT catalog_name FROM postgres_configure_pool(catalog_name='o',max_connections=0); SET pg_oauth_token='fixture-renewed';"
                sql+="SELECT * FROM postgres_query('o','SELECT current_user'); DETACH o;"
                if label=='session-cleared':sql+="SET pg_oauth_token=NULL; ATTACH "+literal(dsn)+" AS o(TYPE POSTGRES); SELECT * FROM postgres_query('o','SELECT current_user'); DETACH o;"
                binary=args.oracle if engine=='cpp' else args.cli
                run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],input=sql,env=env,text=True,capture_output=True,timeout=30)
                (directory/'query.sql').write_text(sql);(directory/'stdout').write_text(run.stdout);(directory/'stderr').write_text(run.stderr)
            sent=[e['token'] for e in endpoint.exchanges if e['kind']=='token']
            passed=run.returncode==0 and run.stdout.strip().endswith('admin') and bool(sent) if error is None else run.returncode!=0 and error in run.stderr
            if error is not None and label not in ['rejected-token','unexpected-final']:passed &= not sent and not any(e['kind']=='password-received' for e in endpoint.exchanges)
            if label=='session-cleared':passed &= sent==['fixture-setting','fixture-env']
            if label=='session-priority':passed &= sent==['fixture-setting']
            if label=='environment-fallback':passed &= sent==['fixture-env']
            if label=='discovery-reconnect':passed &= any(e['kind']=='discovery' for e in endpoint.exchanges) and sent==['fixture-setting']
            if label in ['issuer-mismatch','duplicate-status','nonstring-scope']:passed &= not sent
            if label=='renewed-session':passed &= sent==['fixture-setting','fixture-renewed']
            record=dict(name=label,engine=engine,exit=run.returncode,exchanges=endpoint.exchanges,errors=endpoint.errors,passed=bool(passed and not endpoint.errors and all(e.get('valid',True) for e in endpoint.exchanges)))
            results.append(record);print(json.dumps(record),flush=True)
    if args.slt and args.shim and args.source:
        original=args.source/'test/sql/storage/attach_oauth_token.test'
        manifest=json.loads((Path(__file__).resolve().parents[2]/'extensions/postgres_scanner/upstream.json').read_text())
        assert hashlib.sha256(original.read_bytes()).hexdigest()==manifest['sql_tests']['test/sql/storage/attach_oauth_token.test']
        for engine in ['cpp','rust']:
            directory=out/('original-'+engine);directory.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir();reference=directory/'reference';reference.mkdir();shutil.copy2(original,reference/original.name)
            env={k:v for k,v in os.environ.items() if not k.startswith(('PG','AWS_','RDS_','DUCKDB_AWS_'))}
            with OAuthEndpoint({'fixture-env'}) as endpoint:
                dsn=f"host=127.0.0.1 port={endpoint.port} user=admin dbname=fixture sslmode=disable oauth_issuer={DISCOVERY} oauth_client_id=fixture-client"
                env.update(PGOAUTHTOKEN='fixture-env',PG_OAUTH_DSN=dsn,POSTGRES_TEST_DATABASE_AVAILABLE='1',SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'),SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extension.resolve().parent),TMPDIR=str(directory))
                command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--timeout-secs','30','--out',str(directory/'result.jsonl'),'--log-dir',str(directory/'logs'),original.name]
                if engine=='cpp':command+=['--shim',str(args.shim.resolve())]
                with (directory/'run.log').open('w') as log:subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT)
            rows=[json.loads(l) for l in (directory/'result.jsonl').read_text().splitlines()]
            results.append(dict(name='unchanged-original',engine=engine,rows=rows,passed=len(rows)==1 and rows[0]['status']=='pass' and not endpoint.errors));print(json.dumps(results[-1]),flush=True)
    (out/'verification.json').write_text(json.dumps(dict(cases=results,scope='Owned loopback OAUTHBEARER fixture; no real OAuth issuer',sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['cli','oracle','extension']}),indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
