#!/usr/bin/env python3
"""C++/Rust PostgreSQL secret storage in both directions over one owned database."""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
import subprocess
from cluster import Cluster
from codec_control import literal


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:
        parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args(); out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    httpfs=args.extension.resolve().with_name('httpfs.duckdb_extension')
    assert httpfs.is_file()
    results=[]
    with Cluster(out/'cluster') as cluster:
        def execute(engine,label,sql):
            secrets=out/(label+'-secrets');secrets.mkdir()
            load=(f"LOAD {literal(str(args.extension.resolve()))}; LOAD {literal(str(httpfs))};" if engine=='cpp' else 'LOAD postgres; LOAD httpfs;')
            statement=f"SET secret_directory={literal(str(secrets))};"+load+sql
            (out/(label+'.sql')).write_text(statement)
            binary=args.oracle if engine=='cpp' else args.cli
            run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-nullvalue','__NULL__','-init','/dev/null',':memory:'],input=statement,env=cluster.environment,text=True,capture_output=True,timeout=60)
            (out/(label+'.stdout')).write_text(run.stdout);(out/(label+'.stderr')).write_text(run.stderr)
            return run,list(csv.reader(io.StringIO(run.stdout)))
        for writer,reader in [('cpp','rust'),('rust','cpp')]:
            cluster.reset();tag=writer+'-to-'+reader
            attach="ATTACH 'dbname=postgresscanner' AS vault(TYPE POSTGRES,SECRET_STORAGE_TABLE duckdb_secrets);"
            create="""
CREATE SECRET http_item IN postgres_vault(TYPE HTTP,BEARER_TOKEN 'fixture-token',VERIFY_SSL false,EXTRA_HTTP_HEADERS MAP {'X-Quote':'a,b','X-Number':'7'},SCOPE ['http://fixture/','http://other/']);
CREATE SECRET config_item IN postgres_vault(TYPE POSTGRES,DBNAME 'postgresscanner',PASSWORD 'fixture-only',PORT 5432);
CREATE SECRET s3_item IN postgres_vault(TYPE S3,KEY_ID 'fixture-key',SECRET 'fixture-secret',REGION 'us-east-1',USE_SSL false);
"""
            select="SELECT name,type,provider,persistent,storage,scope,secret_string FROM duckdb_secrets() WHERE storage='postgres_vault' ORDER BY name;"
            first,expected=execute(writer,tag+'-write',attach+create+select)
            second,actual=execute(reader,tag+'-read',"ATTACH 'dbname=postgresscanner' AS vault(TYPE POSTGRES);"+select+"CREATE SECRET added IN postgres_vault(TYPE HTTP,VERIFY_SSL false);")
            third,added=execute(writer,tag+'-read-back',"ATTACH 'dbname=postgresscanner' AS vault(TYPE POSTGRES); SELECT name,type FROM duckdb_secrets() WHERE name='added';")
            # CLI CREATE SECRET reports a one-column Success result; compare the
            # selected seven-column catalog result set separately, retaining raw output.
            expected=[row for row in expected if len(row)==7]
            actual=[row for row in actual if len(row)==7]
            added=[row for row in added if len(row)==2]
            row=dict(direction=tag,writer_exit=first.returncode,reader_exit=second.returncode,read_back_exit=third.returncode,passed=first.returncode==second.returncode==third.returncode==0 and len(expected)==3 and expected==actual and added==[['added','http']])
            results.append(row);print(json.dumps(row),flush=True)
    report=dict(cases=results,server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['cli','oracle','extension']},httpfs_sha256=hashlib.sha256(httpfs.read_bytes()).hexdigest())
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
