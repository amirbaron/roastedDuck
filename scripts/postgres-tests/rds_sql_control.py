#!/usr/bin/env python3
"""Run RDS provider diagnostics without credentials, profiles, or network providers."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','extensions','out']:parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);reference=out/'reference';reference.mkdir()
    source=Path(__file__).with_name('rds_sql.test');shutil.copy2(source,reference/source.name)
    (out/'config').write_text('');(out/'credentials').write_text('');results=[]
    for engine in ['cpp','rust']:
        directory=out/engine;directory.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir()
        env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','RDS_','PG','DUCKDB_AWS_'))}
        env.update(AWS_CONFIG_FILE=str(out/'config'),AWS_SHARED_CREDENTIALS_FILE=str(out/'credentials'),AWS_EC2_METADATA_DISABLED='true',SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'),SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),TMPDIR=str(directory),ROASTEDDUCK_RDS_FIXTURE_EMPTY_ENV='1')
        command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--out',str(directory/'result.jsonl'),'--log-dir',str(directory/'logs'),source.name]
        if engine=='cpp':command+=['--shim',str(args.shim.resolve())]
        with (directory/'run.log').open('w') as log:subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
        rows=[json.loads(l) for l in (directory/'result.jsonl').read_text().splitlines()]
        results.append(dict(engine=engine,rows=rows,passed=len(rows)==1 and rows[0]['status']=='pass'));print(json.dumps(results[-1]),flush=True)
    report=dict(cases=results,sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['slt','shim']},test_sha256=hashlib.sha256(source.read_bytes()).hexdigest())
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)
if __name__=='__main__':main()
