#!/usr/bin/env python3
"""Differential Azure chunk geometry, bounded concurrency and ETag race checks."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import threading
import time

from dfs_fixture import DfsFixture

DATA=bytes(range(256))*64


class TransferFixture(DfsFixture):
    def __init__(self, mutate=False):
        super().__init__()
        self.entries['input.bin']=DATA
        self.mutate=mutate
        self.active=self.peak=0
        self.counter_lock=threading.Lock()

    def before_request(self,handler,parsed,body):
        if handler.command=='GET' and parsed.path.endswith('/input.bin'):
            handler.transfer_read=True
            with self.counter_lock:
                self.active+=1
                self.peak=max(self.peak,self.active)
            time.sleep(0.03)

    def after_response(self,handler,status):
        if getattr(handler,'transfer_read',False):
            handler.transfer_read=False
            if self.mutate and status==206:
                self.mutate=False
                self.etag='"replaced-etag"'
                self.entries['input.bin']=bytes(b^255 for b in DATA)
            with self.counter_lock:
                self.active-=1


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:
        parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args()
    out=args.out.resolve()
    out.mkdir(parents=True,exist_ok=False)
    env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','AZURE_'))
         and k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
    env['TMPDIR']=str(out)
    rows=[]
    cases=[('default',8388608,8388608,5,False),('chunked',4096,1024,2,False),
           ('wide-buffer',32768,4096,3,False),('unbuffered',0,1024,2,False),
           ('zero-concurrency',32768,1024,0,False),('negative-concurrency',32768,1024,-2,False),
           ('etag-race',32768,1024,2,True)]
    for engine,cli in [('cpp',args.oracle),('rust',args.cli)]:
        for scheme in ['az','abfss']:
            for name,buffer,chunk,concurrency,mutate in cases:
                with TransferFixture(mutate) as fixture:
                    startup=f"LOAD '{args.extension.resolve()}'; " if engine=='cpp' else 'LOAD azure; '
                    sql=startup+f"SET allow_persistent_secrets=false; CREATE SECRET s(TYPE azure, CONNECTION_STRING '{fixture.connection}'); SET azure_read_buffer_size={buffer}; SET azure_read_transfer_chunk_size={chunk}; SET azure_read_transfer_concurrency={concurrency}; SELECT octet_length(content), content=from_hex('{DATA.hex()}') FROM read_blob('{scheme}://fixture/input.bin');"
                    run=subprocess.run([str(cli.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],
                                       input=sql,text=True,capture_output=True,env=env,timeout=30)
                    reads=[r for r in fixture.requests if r['method']=='GET']
                    lengths=[int(r['range'].split('-')[1])-int(r['range'].split('=')[1].split('-')[0])+1 for r in reads]
                    success=(run.returncode!=0 and 'ConditionNotMet' in run.stderr) if mutate else (
                        run.returncode==0 and run.stdout.strip().splitlines()==['true',f'{len(DATA)},true'])
                    valid=success and bool(reads) and all(r['valid_signature'] for r in fixture.requests)
                    valid=valid and max(lengths)<=chunk and fixture.peak<=max(1,concurrency)
                    if not mutate and len(reads)>2 and concurrency>1:
                        valid=valid and fixture.peak>1
                    # A multi-chunk range pins all chunks after its initial GET.
                    valid=valid and (len(reads)==1 or any(r['if_match']=='"fixture-etag"' for r in reads))
                    rows.append(dict(engine=engine,scheme=scheme,case=name,passed=valid,exit=run.returncode,
                                     stdout=run.stdout,stderr=run.stderr,peak=fixture.peak,requests=fixture.requests))
                    (out/'progress.json').write_text(json.dumps(rows,indent=2)+'\n')
    report=dict(rows=rows,sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest()
                                  for name in ['cli','oracle','extension']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({e:dict(passed=sum(r['passed'] for r in rows if r['engine']==e),
                             failed=[r['scheme']+'-'+r['case'] for r in rows if r['engine']==e and not r['passed']])
                      for e in ['cpp','rust']}))
    if not all(r['passed'] for r in rows):
        raise SystemExit(1)


if __name__=='__main__':
    main()
