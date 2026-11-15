#!/usr/bin/env python3
"""A stopped standalone COPY must close without draining a deliberately slow tail."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time
from cluster import Cluster,DATABASE
from codec_control import literal

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);results=[]
    with Cluster(out/'cluster') as cluster:
        for engine in ['cpp','rust']:
            cluster.reset()
            cluster.sql("CREATE VIEW slow_tail AS SELECT i FROM generate_series(1,16384) i CROSS JOIN LATERAL pg_sleep(CASE WHEN i>4096 THEN 0.003 ELSE 0 END);",database=DATABASE)
            secrets=out/(engine+'-secrets');secrets.mkdir()
            load='LOAD '+literal(str(args.extension.resolve()))+';' if engine=='cpp' else 'LOAD postgres;'
            sql='SET secret_directory='+literal(str(secrets))+';'+load+" PRAGMA disable_optimizer; SELECT * FROM postgres_scan('dbname=postgresscanner','public','slow_tail') LIMIT 1;"
            binary=args.oracle if engine=='cpp' else args.cli
            start=time.monotonic()
            run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],input=sql,env=cluster.environment,text=True,capture_output=True,timeout=60)
            elapsed=time.monotonic()-start
            (out/(engine+'.sql')).write_text(sql);(out/(engine+'.stdout')).write_text(run.stdout);(out/(engine+'.stderr')).write_text(run.stderr)
            results.append(dict(engine=engine,exit=run.returncode,elapsed_seconds=elapsed,passed=run.returncode==0 and run.stdout.strip()=='1' and elapsed<5))
            print(json.dumps(results[-1]),flush=True)
    (out/'verification.json').write_text(json.dumps(dict(cases=results,server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['cli','oracle','extension']}),indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
