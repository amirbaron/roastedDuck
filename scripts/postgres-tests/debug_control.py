#!/usr/bin/env python3
"""Verify query-debug stream, COPY/type queries, reset and process-global scope."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from cluster import Cluster,DATABASE
from codec_control import literal

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','slt','shim','out']:parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);results=[]
    with Cluster(out/'cluster') as cluster:
        cluster.reset();cluster.sql("CREATE TYPE debug_mood AS ENUM ('one','two'); CREATE TABLE debug_enum_rows(m debug_mood); INSERT INTO debug_enum_rows VALUES('one');",database=DATABASE)
        for engine in ['cpp','rust']:
            for case,body in [
                ('toggle',"SET pg_debug_show_queries=true; CALL postgres_execute('p','SELECT 771003'); RESET pg_debug_show_queries; CALL postgres_execute('p','SELECT 771004');"),
                ('copy',"SET pg_debug_show_queries=true; SELECT * FROM postgres_query('p','SELECT 771005 AS n'); SET pg_debug_show_queries=false;"),
                ('metadata',"SET pg_debug_show_queries=true; SELECT * FROM p.debug_enum_rows; SET pg_debug_show_queries=false;"),
            ]:
                directory=out/(engine+'-'+case);directory.mkdir();(directory/'secrets').mkdir()
                load='LOAD '+literal(str(args.extension.resolve()))+';' if engine=='cpp' else 'LOAD postgres;'
                sql='SET secret_directory='+literal(str(directory/'secrets'))+';'+load+" ATTACH 'dbname=postgresscanner' AS p(TYPE POSTGRES);"+body
                binary=args.oracle if engine=='cpp' else args.cli
                run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],input=sql,env=cluster.environment,text=True,capture_output=True,timeout=30)
                (directory/'query.sql').write_text(sql);(directory/'stdout').write_text(run.stdout);(directory/'stderr').write_text(run.stderr)
                passed=run.returncode==0
                if case=='toggle':passed &= run.stderr.count('SELECT 771003\n\n')==1 and '771004' not in run.stderr and '771003' not in run.stdout and 'DISCARD ALL' not in run.stderr
                elif case=='copy':passed &= run.stdout.strip()=='771005' and run.stderr.count('771005')==1 and 'COPY (' in run.stderr
                else:passed &= run.stdout.strip()=='one' and 'pg_enum' in run.stderr and 'debug_enum_rows' in run.stderr
                record=dict(engine=engine,case=case,passed=bool(passed));results.append(record);print(json.dumps(record),flush=True)
            directory=out/(engine+'-global');directory.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir();(directory/'reference').mkdir()
            (directory/'reference/global.test').write_text("""require postgres_scanner

statement ok a
ATTACH 'dbname=postgresscanner' AS p(TYPE POSTGRES)

statement ok a
SET pg_debug_show_queries=true

query I b
SELECT current_setting('pg_debug_show_queries')
----
false

statement ok b
CALL postgres_execute('p','SELECT 771001')

statement ok a
SET pg_debug_show_queries=false

statement ok b
CALL postgres_execute('p','SELECT 771002')
""")
            env=dict(cluster.environment,SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'),SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extension.resolve().parent))
            command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(directory/'reference'),'--jobs','1','--out',str(directory/'result.jsonl'),'--log-dir',str(directory/'logs'),'global.test']
            if engine=='cpp':command+=['--shim',str(args.shim.resolve())]
            with (directory/'run.log').open('w') as log:subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=30)
            rows=[json.loads(l) for l in (directory/'result.jsonl').read_text().splitlines()];logs=''.join(p.read_text() for p in (directory/'logs').rglob('*') if p.is_file())
            record=dict(engine=engine,case='process-global',rows=rows,passed=len(rows)==1 and rows[0]['status']=='pass' and 'SELECT 771001' in logs and 'SELECT 771002' not in logs);results.append(record);print(json.dumps(record),flush=True)
    (out/'verification.json').write_text(json.dumps(dict(cases=results,server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['cli','oracle','slt','shim','extension']}),indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
