#!/usr/bin/env python3
"""Run unchanged HTTPFS metadata/secret originals with owned persistent storage."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile

ROOT=Path(__file__).resolve().parents[2]
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','source','extensions','out']:parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--engine',choices=['cpp','rust','both'],default='both');parser.add_argument('--file',action='append')
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    manifest=json.loads((ROOT/'extensions/httpfs/upstream.json').read_text());source=args.source.resolve()
    archive=source.parent.parent/'httpfs.tar.gz';assert hashlib.sha256(archive.read_bytes()).hexdigest()==manifest['archive_sha256']
    files=[]
    for name in manifest['sql_tests']:
        if '/secret/' not in name and '/secrets/' not in name:continue
        requires=[l for l in (source/name).read_text().splitlines() if l.startswith('require-env')]
        if all(l=='require-env TEST_PERSISTENT_SECRETS_AVAILABLE' for l in requires):files.append(name)
    if args.file:
        assert set(args.file)<=set(files);files=args.file
    data_hashes={}
    with tarfile.open(archive) as tar:
        for member in tar:
            if member.isfile() and '/data/' in member.name:
                relative=member.name.split('/',1)[1];content=tar.extractfile(member).read();assert content==(source/relative).read_bytes();data_hashes[relative]=hashlib.sha256(content).hexdigest()
    report={}
    for engine in ['cpp','rust'] if args.engine=='both' else [args.engine]:
        directory=out/engine;directory.mkdir();reference=directory/'reference';reference.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir();(directory/'hf').mkdir()
        for name in files:
            content=(source/name).read_bytes();assert hashlib.sha256(content).hexdigest()==manifest['sql_tests'][name];destination=reference/name;destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(content)
        shutil.copytree(source/'data',reference/'data')
        for path in (reference/'data/secrets').rglob('*'):path.chmod(0o700 if path.is_dir() else 0o600)
        env={k:v for k,v in os.environ.items() if not k.startswith(('AWS_','DUCKDB_','SLT_','AZURE_','HF_','HUGGING','DYLD_')) and k!='LOCAL_EXTENSION_REPO' and k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
        (directory/'hf/token').write_text('owned-fixture-token')
        env.update(TMPDIR=str(directory),TZ='UTC',AWS_EC2_METADATA_DISABLED='true',HF_HOME=str(directory/'hf'),HF_TOKEN_PATH=str(directory/'hf/token'),TEST_PERSISTENT_SECRETS_AVAILABLE='1',SLT_ORACLE_EXTENSION_DIRECTORY=str(args.extensions.resolve()),SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'))
        cmd=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),'--jobs','1','--timeout-secs','120','--require','httpfs','--out',str(directory/'results.jsonl'),'--log-dir',str(directory/'logs'),*files]
        if engine=='cpp':cmd+=['--shim',str(args.shim.resolve())]
        with (directory/'run.log').open('w') as log:run=subprocess.run(cmd,env=env,stdout=log,stderr=subprocess.STDOUT)
        rows=[json.loads(l) for l in (directory/'results.jsonl').read_text().splitlines()];assert len(rows)==len(files)
        report[engine]=dict(exit=run.returncode,rows=rows,counts=dict(Counter(r['status'] for r in rows)));print(engine,report[engine]['counts'],flush=True)
        (out/'progress.json').write_text(json.dumps(report,indent=2)+'\n')
    report['data_hashes']=data_hashes;report['scope']='Unchanged metadata and secret originals; historical secrets copied byte-for-byte with required private permissions'
    report['sha256']={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['slt','shim']}
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if any(c['status']!='pass' for engine in ['cpp','rust'] if engine in report for c in report[engine]['rows']):raise SystemExit(1)
if __name__=='__main__':main()
