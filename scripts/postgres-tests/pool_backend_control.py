#!/usr/bin/env python3
"""Exercise real pool lease lifecycle on a disposable local PostgreSQL server."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from cluster import Cluster,DATABASE


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe',required=True,type=Path);parser.add_argument('--out',required=True,type=Path)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    with Cluster(out/'cluster') as cluster:
        cluster.reset()
        run=subprocess.run([str(args.probe.resolve())],env=cluster.environment,text=True,capture_output=True,timeout=30)
        (out/'probe.stdout').write_text(run.stdout);(out/'probe.stderr').write_text(run.stderr)
        idle=cluster.sql("SELECT count(*) FROM pg_stat_activity WHERE datname='postgresscanner' AND state='idle in transaction'",database=DATABASE).strip()
    warnings=(out/'cluster/postgres.log').read_text().count('there is no transaction in progress')
    report=dict(spurious_rollback_warnings=warnings,exit=run.returncode,checks=[line.removeprefix('PASS ') for line in run.stdout.splitlines() if line.startswith('PASS ')],passed=run.returncode==0 and run.stdout.rstrip().endswith('pool-probe-ok') and idle=='0' and warnings==0,idle_in_transaction=idle,server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],probe_sha256=hashlib.sha256(args.probe.read_bytes()).hexdigest())
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))
    if not report['passed'] or not report['server_stopped']:raise SystemExit(1)


if __name__=='__main__':main()
