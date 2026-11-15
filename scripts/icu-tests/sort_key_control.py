#!/usr/bin/env python3
"""Compare every available ICU locale's exact keys on bounded Unicode fixtures."""
import argparse
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import random
import subprocess
import unicodedata

def literal(s):return "'"+s.replace("'","''")+"'"
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for n in ['cli','oracle','out']:parser.add_argument('--'+n,type=Path,required=True)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    env=dict(os.environ,TMPDIR=str(out),TZ='UTC')
    inventory=subprocess.run([str(args.oracle.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:','SELECT collname FROM pragma_collations() ORDER BY 1'],text=True,capture_output=True,env=env,check=True)
    locales=[r[0] for r in csv.reader(io.StringIO(inventory.stdout)) if r[0] not in ['nocase','noaccent','nfc']]
    extra=['de_DE@collation=phonebook','en_US@colNumeric=yes','en-u-kn-true','zh_Hant','tr_TR','fr_FR','root']
    locales=list(dict.fromkeys(locales+extra));rng=random.Random(1506)
    values={'','A','a','\0','a\0b','É','e\u0301','Straße','STRASSE','Iİıi','Σσς','東京','עברית','العربية','हिन्दी','👩\u200d💻','\ufeff','\uffff','a\u0f73\u0f75\u0f81','\u0301\u0323\u0315','2','02','10','a-10','a2'}
    alphabet=list('AaZz0123456789 -_')+list('éßİıΣσς東京中文עבריתالعربية')+list('\u0301\u0323\u0315\u0f73\u0f75\u0f81\u200d\ufe0f')
    for _ in range(256):
        text=''.join(rng.choice(alphabet) if rng.random()<.8 else chr(rng.choice([rng.randrange(0,0xd800),rng.randrange(0xe000,0x110000)])) for _ in range(rng.randrange(1,25)))
        values.update([text,unicodedata.normalize('NFC',text),unicodedata.normalize('NFD',text)])
    values=sorted(values);rows=[f"({i},decode(from_hex('{s.encode().hex()}')))" for i,s in enumerate(values)];rows.append(f'({len(values)},NULL::VARCHAR)')
    setup='CREATE TABLE input(i INTEGER,v VARCHAR); INSERT INTO input VALUES '+','.join(rows)+';\n'
    sql=setup+''.join('SELECT '+literal(loc)+",i,icu_sort_key(v,"+literal(loc)+") FROM input ORDER BY i;\n" for loc in locales)
    (out/'query.sql').write_text(sql);(out/'inputs.json').write_text(json.dumps(dict(locales=locales,strings=values),ensure_ascii=True,indent=2)+'\n')
    results={}
    for engine,binary in [('cpp',args.oracle),('rust',args.cli)]:
        with (out/(engine+'.csv')).open('w') as stdout,(out/(engine+'.stderr')).open('w') as stderr:
            run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-nullvalue','__NULL__','-init','/dev/null',':memory:'],input=sql,text=True,env=env,stdout=stdout,stderr=stderr,timeout=120)
        data=(out/(engine+'.csv')).read_bytes();count=len(list(csv.reader(io.StringIO(data.decode()))))
        results[engine]=dict(exit=run.returncode,rows=count,sha256=hashlib.sha256(data).hexdigest());print(engine,results[engine],flush=True)
    expected=len(locales)*(len(values)+1);passed=all(r['exit']==0 and r['rows']==expected for r in results.values()) and results['cpp']['sha256']==results['rust']['sha256']
    report=dict(passed=passed,locales=len(locales),strings=len(values),keys=expected,results=results,sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['cli','oracle']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not passed:raise SystemExit(1)
if __name__=='__main__':main()
