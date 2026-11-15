#!/usr/bin/env python3
"""Run unchanged PostgreSQL extension SQL with a fresh database per original file."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile

from cluster import Cluster, DATABASE, TEMPLATE, PASSWORD

ROOT=Path(__file__).resolve().parents[2]


def seed(cluster, source, oracle, directory, dataset_extensions):
    exported=directory/'exported'
    path=str(exported).replace("'","''")
    startup='SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; '
    for name in ['tpch','tpcds']:
        extension=str((dataset_extensions/(name+'.duckdb_extension')).resolve()).replace("'","''")
        startup+=f"LOAD '{extension}'; "
    sql=startup+f"CREATE SCHEMA tpch; CREATE SCHEMA tpcds; CALL dbgen(sf=0.01,schema='tpch'); CALL dsdgen(sf=0.01,schema='tpcds'); EXPORT DATABASE '{path}';"
    run=subprocess.run([str(oracle.resolve()),'-batch','-bail','-init','/dev/null',':memory:'],
                       input=sql,text=True,capture_output=True,env=cluster.environment,timeout=120)
    (directory/'export.stdout').write_text(run.stdout)
    (directory/'export.stderr').write_text(run.stderr)
    if run.returncode:
        raise RuntimeError('Oracle dataset export failed')
    cluster.sql((exported/'schema.sql').read_text(),database=TEMPLATE,label='seed-schema')
    # Client-side COPY preserves the exported data while avoiding server-file privileges.
    lines=[]
    for line in (exported/'load.sql').read_text().splitlines():
        if not line.strip():
            continue
        assert line.lstrip().startswith('COPY '),line
        lines.append('\\copy '+line.lstrip()[5:].rstrip().removesuffix(';'))
    cluster.sql('\n'.join(lines)+'\n',database=TEMPLATE,label='seed-copy')
    for filename in ['all_pg_types.sql','decimals.sql','other.sql']:
        cluster.sql((source/'test'/filename).read_text(),database=TEMPLATE,label='seed-'+filename)
    cluster.sql('VACUUM;',database=TEMPLATE,label='seed-vacuum')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['slt','shim','oracle','source','extension','dataset-extensions','out']:
        parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--archive',type=Path,help='Pinned source archive (defaults beside the extracted source directory)')
    parser.add_argument('--engine',choices=['cpp','rust','both'],default='cpp')
    parser.add_argument('--file',action='append')
    parser.add_argument('--all',action='store_true')
    parser.add_argument('--slow',action='store_true',help='Enable POSTGRES_TEST_SLOW for an explicitly selected cohort')
    parser.add_argument('--duckdb-source',type=Path,help='Pinned DuckDB 1.5.6 source for unchanged TPCH/TPCDS expected-result files')
    args=parser.parse_args()
    if args.all and args.file:
        parser.error('--all and --file are mutually exclusive')
    out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    manifest=json.loads((ROOT/'extensions/postgres_scanner/upstream.json').read_text())
    files=list(manifest['sql_tests']) if args.all else (args.file or [
        'test/sql/scanner/types.test','test/sql/scanner/arrays.test','test/sql/scanner/ssl.test',
        'test/sql/storage/attach_create_uppercase_names.test'])
    archive=args.archive or args.source.parent.parent/'postgres_scanner.tar.gz'
    assert hashlib.sha256(archive.read_bytes()).hexdigest()==manifest['archive_sha256']
    fixture_hashes={}
    with tarfile.open(archive,'r:gz') as packed:
        for name in ['test/all_pg_types.sql','test/decimals.sql','test/other.sql']:
            member=next(m for m in packed if m.name.endswith('/'+name))
            data=packed.extractfile(member).read()
            assert data==(args.source/name).read_bytes(),name
            fixture_hashes[name]=hashlib.sha256(data).hexdigest()
    reference=out/'reference'
    needs_duckdb_source=False
    for name in files:
        data=(args.source/name).read_bytes()
        assert hashlib.sha256(data).hexdigest()==manifest['sql_tests'][name],name
        target=reference/name;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(data)
        needs_duckdb_source |= b'<FILE>:duckdb/' in data
    answer_hashes={}
    if needs_duckdb_source:
        if args.duckdb_source is None:
            parser.error('--duckdb-source is required by the selected originals')
        duckdb_source=args.duckdb_source.resolve()
        for folder in ['extension/tpch/dbgen/answers/sf0.01','extension/tpcds/dsdgen/answers/sf0.01']:
            directory=duckdb_source/folder
            if not directory.is_dir():
                parser.error(f'missing pinned answer directory: {directory}')
            for answer in directory.glob('*.csv'):
                answer_hashes[str(answer.relative_to(duckdb_source))]=hashlib.sha256(answer.read_bytes()).hexdigest()
        (reference/'duckdb').symlink_to(duckdb_source,target_is_directory=True)
    extensions=out/'extensions';extensions.mkdir()
    (extensions/'postgres_scanner.duckdb_extension').symlink_to(args.extension.resolve())
    for name in ['tpch','tpcds']:
        (extensions/(name+'.duckdb_extension')).symlink_to((args.dataset_extensions/(name+'.duckdb_extension')).resolve())
    report={}
    explicit_uri_fixture='test/sql/storage/attach_secret.test' in files
    with Cluster(out/'cluster',fixed_port=5432 if explicit_uri_fixture else None,
                 password='postgres' if explicit_uri_fixture else PASSWORD,
                 checkpoint_role='test/sql/storage/attach_checkpoint.test' in files) as cluster:
        seed(cluster,args.source,args.oracle,out,args.dataset_extensions)
        env=dict(cluster.environment,POSTGRES_TEST_DATABASE_AVAILABLE='1',
                 SLT_ORACLE_EXTENSION_DIRECTORY=str(extensions),TMPDIR=str(cluster.tmp))
        env.pop('LOCAL_EXTENSION_REPO',None)
        if args.all or args.slow:
            env['POSTGRES_TEST_SLOW']='1'
        for engine in ['cpp','rust']:
            if args.engine not in ['both',engine]:
                continue
            target=out/engine;target.mkdir();rows=[]
            engine_env=dict(env)
            if engine=='cpp':
                engine_env['SLT_REQUIRE_EXTENSION_PATHS']=json.dumps({name:str((extensions/(name+'.duckdb_extension')).resolve()) for name in ['tpch','tpcds']})
            for index,name in enumerate(files):
                cluster.reset()
                result=target/f'{index}.jsonl'
                command=[str(args.slt.resolve()),'run','--engine',engine,'--root',str(reference),
                         '--jobs','1','--timeout-secs','600','--require','postgres_scanner',
                         '--out',str(result),'--log-dir',str(target/'logs'),name]
                if engine=='cpp':
                    command+=['--shim',str(args.shim.resolve())]
                with (target/f'{index}.log').open('w') as log:
                    subprocess.run(command,env=engine_env,stdout=log,stderr=subprocess.STDOUT)
                rows.extend(json.loads(line) for line in result.read_text().splitlines())
                (target/'progress.json').write_text(json.dumps(rows,indent=2)+'\n')
            report[engine]=dict(rows=rows,counts=dict(Counter(row['status'] for row in rows)))
    report['sha256']={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest()
                      for name in ['slt','shim','oracle','extension']}
    report['fixture_sha256']=fixture_hashes
    report['duckdb_answer_sha256']=answer_hashes
    report['dataset_extension_sha256']={name:hashlib.sha256((args.dataset_extensions/(name+'.duckdb_extension')).read_bytes()).hexdigest() for name in ['tpch','tpcds']}
    report['scope']='Disposable local PostgreSQL; original SQL unchanged; separate restored database per file'
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({engine:r['counts'] for engine,r in report.items() if engine in ['cpp','rust']}))
    if any(row['status']!='pass' for engine,r in report.items() if engine in ['cpp','rust'] for row in r['rows']):
        raise SystemExit(1)


if __name__=='__main__':
    main()
