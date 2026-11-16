#!/usr/bin/env python3
"""Qualify the Rust wire backend against a real owned TLS/SCRAM PostgreSQL cluster."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

from cluster import Cluster


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe',required=True,type=Path)
    parser.add_argument('--oracle',required=True,type=Path)
    parser.add_argument('--extension',required=True,type=Path)
    parser.add_argument('--out',required=True,type=Path)
    args=parser.parse_args()
    out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    with Cluster(out/'cluster') as cluster:
        cluster.reset()
        sql=f"LOAD '{args.extension.resolve()}'; ATTACH 'dbname=postgresscanner' AS p(TYPE postgres); SELECT ssl,version FROM postgres_query('p','SELECT ssl,version FROM pg_stat_ssl WHERE pid=pg_backend_pid()');"
        oracle=subprocess.run([str(args.oracle.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],
                              input=sql,text=True,capture_output=True,env=cluster.environment,timeout=30)
        reference_ok=oracle.returncode==0 and oracle.stdout.strip().startswith('true,TLSv1.')
        run=subprocess.run([str(args.probe.resolve())],text=True,capture_output=True,env=cluster.environment,timeout=40)
        report=dict(reference=dict(exit=oracle.returncode,stdout=oracle.stdout,stderr=oracle.stderr,passed=reference_ok),
                    rust=dict(exit=run.returncode,stdout=run.stdout,stderr=run.stderr,passed=run.returncode==0 and run.stdout.strip()=='wire-probe-ok'))
    report['server_stopped']=json.loads((out/'cluster/cluster.json').read_text())['server_stopped']
    report['sha256']={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['probe','oracle','extension']}
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
    if not report['reference']['passed'] or not report['rust']['passed'] or not report['server_stopped']:
        raise SystemExit(1)


if __name__=='__main__':
    main()
