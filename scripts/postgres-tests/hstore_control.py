#!/usr/bin/env python3
"""Deterministic byte/NULL/escape/duplicate hstore comparisons without a PG server."""
import argparse
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import random
import subprocess


def literal(value):
    return "'" + value.replace("'", "''") + "'"


def quoted(value):
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['cli', 'oracle', 'extension', 'out']:
        parser.add_argument('--' + name, required=True, type=Path)
    args = parser.parse_args()
    out = args.out.resolve(); out.mkdir(parents=True, exist_ok=False)
    rng = random.Random(65156)
    tokens = ['', 'a', 'NULL', 'null', 'a b', '"', '\\', '=>', ',', '日本語', 'א', '\n', '\r', '\t', '\v', '\f', '\x01']
    valid = ['', ' ', 'a=>', 'a=>,', 'a=>NULL, a=>"NULL"', 'a=>"NULL", a=>NULL', '\\=a=>q=w', 'a=>b,']
    for _ in range(192):
        pairs = []
        for _ in range(rng.randrange(1, 7)):
            key = rng.choice(tokens)
            value = rng.choice(tokens)
            pairs.append(quoted(key) + rng.choice(['=>', ' => ', '\v=>\f']) + (rng.choice(['NULL', 'NuLl']) if rng.randrange(5)==0 else quoted(value)))
        valid.append(rng.choice([',', ', ', ',\n']).join(pairs))
    rows = ','.join(f'({i},{literal(h)})' for i,h in enumerate(valid))
    keys = ','.join('('+literal(k)+')' for k in tokens)
    sql = f"SELECT id,postgres_hstore_to_json(h),typeof(postgres_hstore_to_json(h)),postgres_hstore_get(h,k) FROM (VALUES {rows}) t(id,h) CROSS JOIN (VALUES {keys}) q(k) ORDER BY id,k; SELECT postgres_hstore_get(NULL,'a'),postgres_hstore_get('a=>1',NULL),postgres_hstore_to_json(NULL); SELECT postgres_hstore_to_json('a=>'||chr(0));"
    malformed = [' =>null', 'a=b', 'aa=>"', 'a=>"x\\', 'a=>b c=>d', 'a', '"a"!', 'a=>x\\', '"a\\']
    results = []
    runs = {}
    for engine,binary in [('cpp',args.oracle),('rust',args.cli)]:
        load = 'LOAD '+literal(str(args.extension.resolve()))+';' if engine=='cpp' else 'LOAD postgres;'
        env = dict(os.environ, TMPDIR=str(out))
        for name,query in [('valid',sql)] + [(f'error-{i}','SELECT postgres_hstore_get('+literal(h)+",'a');") for i,h in enumerate(malformed)]:
            (out/(name+'.'+engine+'.sql')).write_text(load+query)
            run = subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-nullvalue','__NULL__','-init','/dev/null',':memory:'], input=load+query, text=True,capture_output=True,env=env,timeout=60)
            (out/(name+'.'+engine+'.stdout')).write_text(run.stdout)
            (out/(name+'.'+engine+'.stderr')).write_text(run.stderr)
            runs[(engine,name)]=run
    for name in ['valid'] + [f'error-{i}' for i in range(len(malformed))]:
        cpp,rust = runs[('cpp',name)],runs[('rust',name)]
        if name=='valid':
            passed=cpp.returncode==rust.returncode==0 and list(csv.reader(io.StringIO(cpp.stdout)))==list(csv.reader(io.StringIO(rust.stdout)))
        else:
            passed=cpp.returncode!=0 and rust.returncode!=0 and cpp.stderr.splitlines()[0]==rust.stderr.splitlines()[0] and cpp.stderr.startswith('Invalid Input Error: syntax error in hstore')
        results.append(dict(name=name,passed=passed,cpp_exit=cpp.returncode,rust_exit=rust.returncode))
    report=dict(cases=results,valid_hstores=len(valid),keys_per_hstore=len(tokens),sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['cli','oracle','extension']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(results,indent=2))
    if not all(r['passed'] for r in results): raise SystemExit(1)


if __name__ == '__main__':
    main()
