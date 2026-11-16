#!/usr/bin/env python3
"""Verify binary-file access controls and registered HTTP reads on an owned server."""
import argparse
import csv
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import threading


def literal(value):return "'"+str(value).replace("'","''")+"'"


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','httpfs_extension','out']:
        parser.add_argument('--'+name.replace('_','-'),required=True,type=Path)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    payload=b'PGCOPY\n\xff\r\n\0'+bytes(8)+b''.join(struct.pack('>hii',1,4,i) for i in range(17))+b'\xff\xff'
    path=out/'input.bin';path.write_bytes(payload)
    trace=[]
    class Handler(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def respond(self,head):
            if self.path!='/input.bin':self.send_error(404);return
            start,end=0,len(payload)-1
            interval=self.headers.get('Range')
            if interval:
                match=re.fullmatch(r'bytes=(\d+)-(\d*)',interval)
                if not match:self.send_error(416);return
                start=int(match[1]);end=min(end,int(match[2]) if match[2] else end)
            status=206 if interval else 200
            self.send_response(status)
            self.send_header('Content-Length',str(end-start+1));self.send_header('Accept-Ranges','bytes');self.send_header('ETag','"owned-binary-fixture"')
            if interval:self.send_header('Content-Range',f'bytes {start}-{end}/{len(payload)}')
            self.end_headers()
            trace.append(dict(method=self.command,path=self.path,range=interval,status=status))
            if not head:self.wfile.write(payload[start:end+1])
        def do_HEAD(self):self.respond(True)
        def do_GET(self):self.respond(False)
    server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
    server.daemon_threads=True
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    url=f'http://127.0.0.1:{server.server_port}/input.bin'
    env={k:v for k,v in os.environ.items() if k.lower() not in ['http_proxy','https_proxy','all_proxy']}
    env.update(TMPDIR=str(out),TZ='UTC',NO_PROXY='127.0.0.1,localhost')
    (out/'secrets').mkdir()
    startup=f'SET secret_directory={literal(out/"secrets")}; SET autoinstall_known_extensions=false;'
    cases=[
        ('http-read',f"SELECT count(*),sum(i) FROM read_postgres_binary({literal(url)},columns={{i:'INTEGER'}},buffer_size=24);",False),
        ('home-path',f"SET home_directory={literal(out)}; SELECT count(*),sum(i) FROM read_postgres_binary('~/input.bin',columns={{i:'INTEGER'}});",False),
        ('read-disabled',f"SET enable_external_access=false; SELECT * FROM read_postgres_binary({literal(path)},columns={{i:'INTEGER'}});",True),
        ('write-disabled',f"SET enable_external_access=false; COPY (SELECT 42) TO {literal(out/'forbidden.bin')} (FORMAT postgres_binary);",True),
    ]
    results=[];traces={}
    try:
        for name,sql,error in cases:
            runs={}
            for engine in ['cpp','rust']:
                load=f'LOAD {literal(args.httpfs_extension.resolve())}; LOAD {literal(args.extension.resolve())};' if engine=='cpp' else 'LOAD httpfs; LOAD postgres;'
                begin=len(trace)
                binary=args.oracle if engine=='cpp' else args.cli
                (out/(name+'.'+engine+'.sql')).write_text(startup+load+sql)
                run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],input=startup+load+sql,text=True,capture_output=True,env=env,timeout=30)
                (out/(name+'.'+engine+'.stdout')).write_text(run.stdout);(out/(name+'.'+engine+'.stderr')).write_text(run.stderr)
                runs[engine]=run;traces[name+'.'+engine]=trace[begin:]
            if error:
                passed=all(r.returncode!=0 and ('disabled' in r.stderr.lower() or 'permission' in r.stderr.lower()) for r in runs.values())
            else:
                passed=all(r.returncode==0 for r in runs.values()) and list(csv.reader(io.StringIO(runs['cpp'].stdout)))==list(csv.reader(io.StringIO(runs['rust'].stdout)))==[['17','136']]
            if name=='http-read':passed &= all(any(t['method']=='GET' for t in traces[name+'.'+e]) for e in runs)
            results.append(dict(name=name,passed=passed,cpp_exit=runs['cpp'].returncode,rust_exit=runs['rust'].returncode))
            print(json.dumps(results[-1]),flush=True)
    finally:
        server.shutdown();server.server_close();thread.join(timeout=5)
    report=dict(cases=results,server_stopped=not thread.is_alive(),requests=traces,forbidden_output_exists=(out/'forbidden.bin').exists(),sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['cli','oracle','extension','httpfs_extension']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not report['server_stopped'] or report['forbidden_output_exists'] or not all(r['passed'] for r in results):raise SystemExit(1)


if __name__=='__main__':main()
