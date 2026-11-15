#!/usr/bin/env python3
"""Run the pinned engine's unchanged ICU-required SQL inventory, serially."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT=Path(__file__).resolve().parents[2]
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','source','manifest','extensions','dataset-extensions','out']:parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    source=args.source.resolve();manifest=json.loads(args.manifest.read_text());files={}
    for path in (source/'test').rglob('*'):
        if path.is_file() and path.suffix in ['.test','.test_slow'] and any(l.strip()=='require icu' for l in path.read_text(errors='replace').splitlines()):
            name=str(path.relative_to(source));digest=hashlib.sha256(path.read_bytes()).hexdigest();assert digest==manifest[name],name;files[name]=digest
    assert not list((source/'extension/icu').rglob('*.test*')), 'New extension-local tests need inventorying'
    (out/'files.txt').write_text('\n'.join(sorted(files))+'\n');(out/'source-hashes.json').write_text(json.dumps(files,indent=2)+'\n')
    (out/'tmp').mkdir();staged=out/'oracle-extensions';staged.mkdir()
    for directory in [args.extensions,args.dataset_extensions]:
        for extension in directory.glob('*.duckdb_extension'):
            target=staged/extension.name
            if not target.exists():target.symlink_to(extension.resolve())
    repository=out/'repository';repository.mkdir()
    for extension in args.extensions.glob('*.duckdb_extension'):
        with extension.open('rb') as blob:blob.seek(-320,2);platform=blob.read(32).split(b'\0',1)[0].decode()
        destination=repository/'v1.5.6'/platform;destination.mkdir(parents=True,exist_ok=True)
        (destination/extension.name).symlink_to(extension.resolve())
        compressed=extension.with_name(extension.name+'.gz')
        if compressed.exists():(destination/compressed.name).symlink_to(compressed.resolve())
    results={}
    for engine in ['cpp','rust']:
        directory=out/engine;directory.mkdir();(directory/'secrets').mkdir();(directory/'extensions').mkdir()
        env={k:v for k,v in os.environ.items() if not k.startswith(('SLT_','DUCKDB_','DYLD_')) and k!='LOCAL_EXTENSION_REPO'}
        env.update(TMPDIR=str(out/'tmp'),TZ='UTC',SLT_ORACLE_EXTENSION_DIRECTORY=str(staged),SLT_FIXTURE_SECRET_DIRECTORY=str(directory/'secrets'),SLT_FIXTURE_EXTENSION_DIRECTORY=str(directory/'extensions'))
        rows=[];exit_code=0
        groups={False:[],True:[]}
        for name in sorted(files):groups['require-env LOCAL_EXTENSION_REPO' in (source/name).read_text()].append(name)
        for autoload,names in groups.items():
            if not names:continue
            mode='autoload' if autoload else 'normal';listing=directory/(mode+'.txt');listing.write_text('\n'.join(names)+'\n')
            mode_env=dict(env)
            if autoload:mode_env['LOCAL_EXTENSION_REPO']=str(repository)
            command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(source),'--files-from',str(listing),'--jobs','1','--timeout-secs','300','--require','icu','--out',str(directory/(mode+'.jsonl')),'--log-dir',str(directory/(mode+'-logs'))]
            if engine=='cpp':command+=['--shim',str(args.shim.resolve())]
            with (directory/(mode+'.log')).open('w') as log:run=subprocess.run(command,env=mode_env,stdout=log,stderr=subprocess.STDOUT)
            exit_code=exit_code or run.returncode
            current=[json.loads(l) for l in (directory/(mode+'.jsonl')).read_text().splitlines()];assert len(current)==len(names)
            for row in current:row['fixture_mode']=mode
            rows+=current
        (directory/'results.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in rows))
        assert len(rows)==len(files)
        results[engine]=dict(exit=exit_code,counts=dict(Counter(r['status'] for r in rows)),rows=rows);print(engine,results[engine]['counts'],flush=True)
        (out/'progress.json').write_text(json.dumps(results,indent=2)+'\n')
    for name,digest in files.items():assert hashlib.sha256((source/name).read_bytes()).hexdigest()==digest,name
    report=dict(results=results,source_files=len(files),scope='Every pinned engine SQL file requiring ICU; original requirements and SQL retained',sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['slt','shim','manifest']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if any(r['status']!='pass' for e in results.values() for r in e['rows']):raise SystemExit(1)
if __name__=='__main__':main()
