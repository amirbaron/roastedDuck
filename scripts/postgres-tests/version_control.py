#!/usr/bin/env python3
"""Vendor/version and failed-snapshot controls on an owned real PostgreSQL server.

Only functions in the disposable fixture schema are overridden. This is not live
Redshift/Aurora or an old PostgreSQL server qualification.
"""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
import subprocess
from cluster import Cluster,DATABASE
from codec_control import literal

VERSION_QUERY="SELECT version(), (SELECT COUNT(*) FROM pg_settings WHERE name LIKE 'rds%')"

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);results=[]
    scenarios=[
        ('postgres-modern','PostgreSQL 17.11 fixture',0,True,False),
        ('postgres-old','PostgreSQL 9.5.25 fixture',0,False,False),
        ('unknown-modern','OtherVendor 17.11 fixture',0,True,False),
        ('aurora','PostgreSQL 17.11 fixture',1,True,False),
        ('redshift','PostgreSQL 8.0.2 fixture Redshift 1.0',0,False,True),
        ('redshift-overrides-aurora','PostgreSQL 8.0.2 fixture Redshift 1.0',1,False,True),
        ('no-digits','unidentified server',0,False,False),
        ('version-error',None,0,False,False),
        ('snapshot-error','PostgreSQL 17.11 fixture',0,True,False),
    ]
    with Cluster(out/'cluster') as cluster:
        cluster.reset()
        cluster.sql("CREATE SCHEMA fixture; CREATE TYPE public.vendor_mood AS ENUM ('one','two'); CREATE TABLE public.vendor_data AS SELECT i, 'value-'||i AS s FROM generate_series(1,10000) i; ANALYZE public.vendor_data;",database=DATABASE)
        for label,version,rds,parallel,text_mode in scenarios:
            body="BEGIN RAISE EXCEPTION 'fixture version unavailable'; END" if version is None else 'BEGIN RETURN '+literal(version)+'; END'
            snapshot="BEGIN RAISE EXCEPTION 'fixture snapshot unavailable'; END" if label=='snapshot-error' else 'BEGIN RETURN pg_catalog.pg_export_snapshot(); END'
            cluster.sql('CREATE OR REPLACE FUNCTION fixture.version() RETURNS text LANGUAGE plpgsql AS '+literal(body)+'; CREATE OR REPLACE FUNCTION fixture.pg_export_snapshot() RETURNS text LANGUAGE plpgsql AS '+literal(snapshot)+"; CREATE OR REPLACE VIEW fixture.pg_settings AS SELECT 'rds.fixture'::text AS name WHERE "+str(rds)+'>0;',database=DATABASE)
            runs={}
            for engine in ['cpp','rust']:
                directory=out/(label+'-'+engine);directory.mkdir();(directory/'secrets').mkdir()
                dsn="dbname=postgresscanner options='-csearch_path=fixture,pg_catalog,public'"
                load='LOAD '+literal(str(args.extension.resolve()))+';' if engine=='cpp' else 'LOAD postgres;'
                sql='SET secret_directory='+literal(str(directory/'secrets'))+';'+load+"SET pg_debug_show_queries=true; SET threads=4; SET pg_pages_per_task=1; SET pg_use_text_protocol=false; ATTACH "+literal(dsn)+" AS p(TYPE postgres,READ_ONLY); SELECT count(*),sum(i) FROM p.public.vendor_data;"
                if label!='snapshot-error':
                    sql+="SELECT count(*),sum(i) FROM postgres_scan("+literal(dsn)+",'public','vendor_data'); SELECT * FROM postgres_query('p','SELECT i,s FROM public.vendor_data WHERE i<3 ORDER BY i');"
                if label!='snapshot-error':sql+="SELECT count(*) FROM duckdb_types() WHERE database_name='p' AND schema_name='public' AND type_name='vendor_mood';"
                sql+='DETACH p;'
                binary=args.oracle if engine=='cpp' else args.cli
                run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],input=sql,text=True,capture_output=True,env=cluster.environment,timeout=40)
                (directory/'query.sql').write_text(sql);(directory/'stdout').write_text(run.stdout);(directory/'stderr').write_text(run.stderr);runs[engine]=run
            if label=='snapshot-error':
                passed=all(r.returncode!=0 and 'current transaction is aborted' in r.stderr for r in runs.values())
            else:
                expected=[['10000','50005000'],['10000','50005000'],['1','value-1'],['2','value-2'],['0' if label in ('redshift','redshift-overrides-aurora','no-digits','version-error') else '1']]
                passed=all(r.returncode==0 and list(csv.reader(io.StringIO(r.stdout)))==expected for r in runs.values())
                for r in runs.values():
                    passed &= r.stderr.count(VERSION_QUERY)==2 and 'SHOW server_version_num' not in r.stderr
                    passed &= ('ctid BETWEEN' in r.stderr)==parallel
                    passed &= ('COPY (' not in r.stderr)==text_mode
            record=dict(name=label,cpp_exit=runs['cpp'].returncode,rust_exit=runs['rust'].returncode,passed=bool(passed));results.append(record);print(json.dumps(record),flush=True)
    report=dict(cases=results,scope='Owned PostgreSQL with controlled version/snapshot functions; not live vendor or old-server acceptance',server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['cli','oracle','extension']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not report['server_stopped'] or not all(c['passed'] for c in results):raise SystemExit(1)
if __name__=='__main__':main()
