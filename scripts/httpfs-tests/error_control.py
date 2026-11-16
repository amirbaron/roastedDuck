#!/usr/bin/env python3
"""Compare origin/proxy DNS and connection diagnostics on the real SQL path."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import http.server
import threading
import time
import subprocess

def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:p.add_argument('--'+name,type=Path,required=True)
    args=p.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);results=[]
    env={k:v for k,v in os.environ.items() if k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
    class Slow(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_HEAD(self):
            time.sleep(3)
            try:self.send_response(200);self.send_header('Content-Length','4');self.end_headers()
            except (BrokenPipeError,ConnectionResetError):pass
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Slow);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    with socket.socket() as unused:
        unused.bind(('127.0.0.1',0));port=unused.getsockname()[1]
        for implementation in ['curl','httplib']:
            for case,proxy,url in [('origin-dns','', 'http://owned-nxdomain.invalid/a.csv'),('connect-timeout','',f'http://127.0.0.1:{port}/a.csv'),('read-timeout','',f'http://127.0.0.1:{server.server_port}/a.csv'),('proxy-dns','http://owned-proxy.invalid:8123',f'http://127.0.0.1:{port}/a.csv')]:
                errors={}
                for engine,binary in [('cpp',args.oracle),('rust',args.cli)]:
                    directory=out/(implementation+'-'+case+'-'+engine);directory.mkdir()
                    load="LOAD '"+str(args.extension.resolve()).replace("'","''")+"';" if engine=='cpp' else 'LOAD httpfs;'
                    sql=load+f" SET httpfs_client_implementation='{implementation}'; SET http_retries=0; SET http_timeout=1; SET http_proxy='{proxy}'; SELECT * FROM read_csv('{url}');"
                    run=subprocess.run([str(binary.resolve()),'-batch','-bail','-init','/dev/null',':memory:'],input=sql,text=True,capture_output=True,env=dict(env,TMPDIR=str(directory)),timeout=20)
                    (directory/'stdout').write_text(run.stdout);(directory/'stderr').write_text(run.stderr);(directory/'query.sql').write_text(sql)
                    assert run.returncode!=0;errors[engine]=run.stderr.split('\n\n',1)[0].strip()
                record=dict(implementation=implementation,case=case,errors=errors,passed=errors['cpp']==errors['rust']);results.append(record);print(json.dumps(record),flush=True)
    server.shutdown();server.server_close();thread.join()
    (out/'verification.json').write_text(json.dumps(dict(cases=results,sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['cli','oracle','extension']}),indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
