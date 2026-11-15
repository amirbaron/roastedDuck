#!/usr/bin/env python3
"""Interrupt real PostgreSQL reads and COPY inserts through the Rust client API."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from cluster import Cluster,DATABASE

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe',required=True,type=Path)
    parser.add_argument('--out',required=True,type=Path)
    args=parser.parse_args()
    out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    results=[]
    with Cluster(out/'cluster') as cluster:
        cluster.reset()
        secrets=out/'secrets';secrets.mkdir()
        cluster.environment['ROASTEDDUCK_TEST_SECRET_DIRECTORY']=str(secrets)
        cluster.sql("""CREATE VIEW slow_scan AS SELECT 1::integer id FROM pg_sleep(30);
CREATE TABLE slow_insert(id integer);
CREATE FUNCTION pause_insert() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN PERFORM pg_sleep(30); RETURN NEW; END$$;
CREATE TRIGGER pause_insert BEFORE INSERT ON slow_insert FOR EACH ROW EXECUTE FUNCTION pause_insert();
CREATE TABLE slow_mutation(id INTEGER,value INTEGER); INSERT INTO slow_mutation VALUES(1,1);
CREATE FUNCTION pause_mutation() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN PERFORM pg_sleep(30); IF TG_OP='DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF; END$$;
CREATE TRIGGER pause_mutation BEFORE UPDATE OR DELETE ON slow_mutation FOR EACH ROW EXECUTE FUNCTION pause_mutation();""",database=DATABASE)
        for mode in ['legacy','attached','insert','update','delete','merge']:
            run=subprocess.run([str(args.probe.resolve()),mode],env=cluster.environment,text=True,capture_output=True,timeout=40)
            (out/(mode+'.stdout')).write_text(run.stdout);(out/(mode+'.stderr')).write_text(run.stderr)
            results.append(dict(mode=mode,exit=run.returncode,passed=run.returncode==0 and run.stdout.strip()=='interrupt-ok'))
            print(json.dumps(results[-1]),flush=True)
    report=dict(cases=results,server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],probe_sha256=hashlib.sha256(args.probe.read_bytes()).hexdigest())
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not report['server_stopped'] or not all(r['passed'] for r in results):raise SystemExit(1)

if __name__=='__main__':main()
