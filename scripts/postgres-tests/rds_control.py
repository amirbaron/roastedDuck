#!/usr/bin/env python3
"""Simulated RDS cleartext-auth endpoint: validate SigV4 and token reuse on owned loopback sockets."""
import argparse
import hashlib
import hmac
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import threading
import time
from urllib.parse import parse_qsl, quote, urlsplit
from codec_control import literal

I = lambda n: struct.pack('!I', n)
H = lambda n: struct.pack('!H', n)

def recv(sock,n):
    value=b''
    while len(value)<n:
        part=sock.recv(n-len(value))
        if not part: raise EOFError()
        value+=part
    return value

def message(sock,kind,body=b''):
    sock.sendall(kind+I(len(body)+4)+body)

def packet(sock):
    kind=recv(sock,1);return kind,recv(sock,struct.unpack('!I',recv(sock,4))[0]-4)

def validate(token,port):
    parsed=urlsplit('http://'+token);pairs=parse_qsl(parsed.query,keep_blank_values=True);values=dict(pairs)
    assert parsed.netloc==f'127.0.0.1:{port}'
    assert values['Action']=='connect' and values['DBUser']=='postgres'
    assert values['X-Amz-Algorithm']=='AWS4-HMAC-SHA256' and values['X-Amz-Expires']=='900'
    assert values['X-Amz-Security-Token']=='fixture-session'
    credential=values['X-Amz-Credential'].split('/');assert credential[0]=='fixture-key' and credential[2:]==['us-east-1','rds-db','aws4_request']
    encode=lambda s:quote(s,safe='-_.~')
    query='&'.join(k+'='+v for k,v in sorted((encode(k),encode(v)) for k,v in pairs if k!='X-Amz-Signature'))
    canonical='GET\n/\n'+query+'\nhost:'+parsed.netloc+'\n\nhost\n'+hashlib.sha256(b'').hexdigest()
    string='AWS4-HMAC-SHA256\n'+values['X-Amz-Date']+'\n'+'/'.join(credential[1:])+'\n'+hashlib.sha256(canonical.encode()).hexdigest()
    key=b'AWS4fixture-secret'
    for part in credential[1:]: key=hmac.new(key,part.encode(),hashlib.sha256).digest()
    assert hmac.compare_digest(hmac.new(key,string.encode(),hashlib.sha256).hexdigest(),values['X-Amz-Signature']), 'SigV4 signature mismatch'
    return hashlib.sha256(token.encode()).hexdigest()

class Endpoint:
    def __enter__(self):
        self.sock=socket.socket();self.sock.bind(('127.0.0.1',0));self.sock.listen();self.sock.settimeout(.2);self.port=self.sock.getsockname()[1]
        self.stop=threading.Event();self.workers=[];self.tokens=[];self.errors=[];self.queries=[];self.connections=[]
        self.thread=threading.Thread(target=self.accept,daemon=True);self.thread.start();return self
    def accept(self):
        while not self.stop.is_set():
            try:conn,_=self.sock.accept()
            except socket.timeout:continue
            except OSError:return
            self.connections.append(conn);worker=threading.Thread(target=self.client,args=(conn,),daemon=True);self.workers.append(worker);worker.start()
    def shape(self,sql):
        upper=sql.strip().upper()
        if upper.startswith('SHOW SERVER_VERSION_NUM'):return [('server_version_num',25)],['170011'],'SHOW'
        if upper.startswith('SELECT VERSION()'):return [('version',25),('count',20)],['PostgreSQL 17.11 fixture','1'],'SELECT 1'
        if upper.startswith('SELECT TO_REGCLASS'):return [('to_regclass',2205)],[None],'SELECT 1'
        if upper=='SELECT 1':return [('?column?',23)],['1'],'SELECT 1'
        if upper.startswith(('BEGIN','COMMIT','ROLLBACK','DISCARD','SET ')):return [],[],upper.split()[0]
        raise AssertionError('Unexpected fixture SQL: '+sql[:180])
    def describe(self,conn,columns,binary=False):
        body=H(len(columns))
        for name,oid in columns:body+=name.encode()+b'\0'+I(0)+H(0)+I(oid)+struct.pack('!h',4 if oid==23 else 8 if oid==20 else -1)+struct.pack('!i',-1)+H(int(binary))
        message(conn,b'T',body)
    def rows(self,conn,columns,values,binary=False):
        if not columns:return
        body=H(len(values))
        for (_,oid),value in zip(columns,values):
            if value is None:body+=struct.pack('!i',-1)
            else:
                data=struct.pack('!i',int(value)) if binary and oid==23 else struct.pack('!q',int(value)) if binary and oid==20 else value.encode()
                body+=I(len(data))+data
        message(conn,b'D',body)
    def client(self,conn):
        statements={};portals={};state=b'I'
        try:
            conn.settimeout(20);startup=recv(conn,struct.unpack('!I',recv(conn,4))[0]-4)
            if struct.unpack('!I',startup[:4])[0] in (80877103,80877104):
                conn.sendall(b'N');startup=recv(conn,struct.unpack('!I',recv(conn,4))[0]-4)
            assert struct.unpack('!I',startup[:4])[0]==196608
            message(conn,b'R',I(3));kind,password=packet(conn);assert kind==b'p';token=password.rstrip(b'\0').decode();self.tokens.append(validate(token,self.port))
            message(conn,b'R',I(0));message(conn,b'S',b'server_version\x0017.11\0');message(conn,b'S',b'client_encoding\x00UTF8\0');message(conn,b'K',I(1234)+I(5678));message(conn,b'Z',state)
            while True:
                kind,body=packet(conn)
                if kind==b'X':break
                if kind==b'Q':
                    for sql in body.rstrip(b'\0').decode().split(';'):
                        if not sql.strip():continue
                        self.queries.append(sql.strip());columns,values,tag=self.shape(sql)
                        if tag=='BEGIN':state=b'T'
                        if tag in ('COMMIT','ROLLBACK'):state=b'I'
                        if sql.strip().upper()=='SELECT 1' and state==b'T':time.sleep(1.1)
                        if columns:self.describe(conn,columns);self.rows(conn,columns,values)
                        message(conn,b'C',tag.encode()+b'\0')
                    message(conn,b'Z',state)
                elif kind==b'P':
                    name,sql,_=body.split(b'\0',2);statements[name]=sql.decode();message(conn,b'1')
                elif kind==b'D':
                    name=body[1:-1];sql=statements[name] if body[:1]==b'S' else portals[name][0]
                    columns,_,_=self.shape(sql)
                    if body[:1]==b'S':message(conn,b't',H(0))
                    if columns:self.describe(conn,columns)
                    else:message(conn,b'n')
                elif kind==b'B':
                    portal,name,rest=body.split(b'\0',2);formats=struct.unpack('!H',rest[:2])[0];offset=2+2*formats
                    count=struct.unpack('!H',rest[offset:offset+2])[0];offset+=2;assert count==0
                    result_count=struct.unpack('!H',rest[offset:offset+2])[0];binary=result_count>0 and struct.unpack('!H',rest[offset+2:offset+4])[0]==1
                    portals[portal]=(statements[name],binary);message(conn,b'2')
                elif kind==b'E':
                    name=body.split(b'\0',1)[0];sql,binary=portals[name];self.queries.append(sql);columns,values,tag=self.shape(sql);self.rows(conn,columns,values,binary);message(conn,b'C',tag.encode()+b'\0')
                elif kind==b'S':message(conn,b'Z',state)
                elif kind==b'C':message(conn,b'3')
                elif kind==b'H':pass
                else:raise AssertionError('Unexpected message '+repr(kind))
        except (EOFError,ConnectionResetError,BrokenPipeError):pass
        except Exception as error:self.errors.append(str(error))
        finally:conn.close()
    def __exit__(self,*_):
        self.stop.set();self.sock.close()
        for conn in self.connections:
            try:conn.shutdown(socket.SHUT_RDWR)
            except OSError:pass
        self.thread.join(2)
        for worker in self.workers:worker.join(2)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);results=[];diagnostics={}
    for engine in ['cpp','rust']:
        directory=out/engine;directory.mkdir();(directory/'secrets').mkdir();(directory/'config').write_text('');(directory/'credentials').write_text('')
        env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','PG','RDS_','DUCKDB_AWS_'))}
        env.update(AWS_CONFIG_FILE=str(directory/'config'),AWS_SHARED_CREDENTIALS_FILE=str(directory/'credentials'),AWS_EC2_METADATA_DISABLED='true',AWS_ACCESS_KEY_ID='fixture-key',AWS_SECRET_ACCESS_KEY='fixture-secret',AWS_SESSION_TOKEN='fixture-session',TMPDIR=str(directory))
        load='LOAD postgres; LOAD aws;' if engine=='rust' else ''.join('LOAD '+literal(str(args.extension.resolve().with_name(n+'.duckdb_extension')))+';' for n in ['httpfs','postgres_scanner','aws'])
        with Endpoint() as endpoint:
            sql='SET secret_directory='+literal(str(directory/'secrets'))+';'+load+f"CREATE SECRET template(TYPE rds,CHAIN 'env',RDS_USER 'postgres',RDS_HOST '127.0.0.1',RDS_PORT '{endpoint.port}',REGION 'us-east-1'); CREATE SECRET login(TYPE postgres,HOST '127.0.0.1',PORT '{endpoint.port}',USER 'postgres',DBNAME 'fixture',SSLMODE 'disable',AWS_RDS_SECRET 'template'); ATTACH '' AS rds(TYPE postgres,SECRET login); SELECT database_name FROM duckdb_databases() WHERE database_name='rds'; SELECT catalog_name FROM postgres_configure_pool(catalog_name='rds',max_connections=0); CALL postgres_execute('rds','SELECT 1'); CALL postgres_execute('rds','SELECT 1'); SELECT count(*) FROM duckdb_secrets() WHERE name LIKE '%postgres_temp'; DETACH rds;"
            (directory/'query.sql').write_text(sql);binary=args.oracle if engine=='cpp' else args.cli
            run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],input=sql,env=env,text=True,capture_output=True,timeout=40)
            (directory/'stdout').write_text(run.stdout);(directory/'stderr').write_text(run.stderr)
        record=dict(engine=engine,exit=run.returncode,authentications=len(endpoint.tokens),token_hashes=endpoint.tokens,errors=endpoint.errors,queries=endpoint.queries,passed=run.returncode==0 and len(endpoint.tokens)>=3 and len(set(endpoint.tokens))==1 and not endpoint.errors and run.stdout.strip().endswith('0'))
        results.append(record);print(json.dumps(record),flush=True)
        for label,options,expected in [
            ('password-conflict',"PASSWORD 'fixture-rejected',AWS_RDS_SECRET 'missing'","'PASSWORD' parameters must not be specified"),
            ('missing-template',"AWS_RDS_SECRET '  missing  '",'Specified RDS secret with name: "missing" not found'),
            ('unicode-template',"AWS_RDS_SECRET '\u00a0missing\u00a0'",'Specified RDS secret with name:'),
        ]:
            path=directory/label;path.mkdir();(path/'secrets').mkdir()
            sql='SET secret_directory='+literal(str(path/'secrets'))+';'+load+"CREATE SECRET login(TYPE postgres,"+options+"); ATTACH '' AS failed(TYPE postgres,SECRET login);"
            run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],input=sql,env=env,text=True,capture_output=True,timeout=30)
            (path/'query.sql').write_text(sql);(path/'stdout').write_text(run.stdout);(path/'stderr').write_text(run.stderr)
            diagnostics[(engine,label)]=run.stderr
            record=dict(engine=engine,case=label,exit=run.returncode,passed=run.returncode!=0 and expected in run.stderr)
            results.append(record);print(json.dumps(record),flush=True)
    for label in ['password-conflict','missing-template','unicode-template']:
        results.append(dict(case=label+'-oracle-parity',passed=diagnostics[('cpp',label)]==diagnostics[('rust',label)]))
    (out/'verification.json').write_text(json.dumps(dict(cases=results,scope='Simulated loopback RDS SigV4/password protocol and ATTACH; no live AWS service',sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['cli','oracle','extension']}),indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
