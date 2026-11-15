#!/usr/bin/env python3
"""Binary-file byte equality, both read directions, and boundary/error comparisons."""
import argparse
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import subprocess


def literal(value):
    return "'" + str(value).replace("'", "''") + "'"


CRS = 'GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1]],AXIS["geodetic longitude (Lon)",east,ORDER[2]],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4326]]'


CASES = [
    ('scalars', "SELECT true, '-32768'::SMALLINT, '-2147483648'::INTEGER, '-9223372036854775808'::BIGINT, 1.25::FLOAT, -1.25e100::DOUBLE, DATE '0001-01-01 BC', TIME '24:00:00', TIMETZ '12:45:01.123456+05:45', TIMESTAMP '1969-12-31 23:59:59.999999', TIMESTAMPTZ '2024-01-01 01:02:03+02', INTERVAL '-2 months 3 days -04:05:06.123456', UUID 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', 'héllo 🦆', '\\x00\\xFF'::BLOB", ['BOOLEAN','SMALLINT','INTEGER','BIGINT','FLOAT','DOUBLE','DATE','TIME','TIME WITH TIME ZONE','TIMESTAMP','TIMESTAMP WITH TIME ZONE','INTERVAL','UUID','VARCHAR','BLOB']),
    ('decimals', 'SELECT 0::DECIMAL(4,2), 1.20::DECIMAL(4,2), -12345.6700::DECIMAL(18,4), 99999999999999999999999999999999999999::DECIMAL(38,0), 0.00000000000000000000000000000000000001::DECIMAL(38,38), 0::DECIMAL(38,38)', ['DECIMAL(4,2)','DECIMAL(4,2)','DECIMAL(18,4)','DECIMAL(38,0)','DECIMAL(38,38)','DECIMAL(38,38)']),
    ('arrays', "SELECT [1,NULL,-2]::INTEGER[], []::INTEGER[], ['é','a,b',NULL]::VARCHAR[], [[1,2],[3,4]]::INTEGER[][], [[]]::INTEGER[][], [[[]]]::INTEGER[][][]", ['INTEGER[]','INTEGER[]','VARCHAR[]','INTEGER[][]','INTEGER[][]','INTEGER[][][]']),
    ('struct', "SELECT {'i':42,'s':'héllo','ids':[1,NULL,3]}::STRUCT(i INTEGER,s VARCHAR,ids INTEGER[])", ['STRUCT(i INTEGER,s VARCHAR,ids INTEGER[])']),
    ('enum', "SELECT 'a''b'::ENUM('a''b','x')", ["ENUM('a''b','x')"]),
    ('geometry', f"SELECT 'POINT (1.25 -2)'::GEOMETRY, 'POINT (1 2)'::GEOMETRY({literal(CRS)})", ['GEOMETRY',f"GEOMETRY({literal(CRS)})"]),
    ('json', "SELECT '{\"a\":1}'::JSON", ['JSON']),
    ('nulls', 'SELECT NULL::INTEGER, NULL::UINTEGER, NULL::TINYINT, NULL::INTEGER[], NULL::STRUCT(i INTEGER)', ['INTEGER','UINTEGER','TINYINT','INTEGER[]','STRUCT(i INTEGER)']),
    ('infinity', "SELECT DATE 'infinity', DATE '-infinity', TIMESTAMP 'infinity', TIMESTAMP '-infinity', 'Infinity'::DOUBLE, '-Infinity'::FLOAT", ['DATE','DATE','TIMESTAMP','TIMESTAMP','DOUBLE','FLOAT']),
    ('many', 'SELECT i::INTEGER FROM range(4097)t(i) ORDER BY i', ['INTEGER']),
    ('empty', 'SELECT 42::UINTEGER WHERE false', ['UINTEGER']),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:
        parser.add_argument('--'+name, required=True, type=Path)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    env=dict(os.environ,TMPDIR=str(out),TZ='UTC')
    results=[]
    def run(engine,name,sql):
        load='LOAD '+literal(args.extension.resolve())+';' if engine=='cpp' else 'LOAD postgres;'
        binary=args.oracle if engine=='cpp' else args.cli
        full=load+sql
        (out/(name+'.'+engine+'.sql')).write_text(full)
        result=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-nullvalue','__NULL__','-init','/dev/null',':memory:'],input=full,text=True,capture_output=True,env=env,timeout=60)
        (out/(name+'.'+engine+'.stdout')).write_text(result.stdout);(out/(name+'.'+engine+'.stderr')).write_text(result.stderr)
        return result
    def save(name,passed,**extra):
        results.append(dict(name=name,passed=passed,**extra));print(json.dumps(results[-1]),flush=True)
        (out/'progress.json').write_text(json.dumps(results,indent=2)+'\n')
    def compare(name,sql,error=False):
        runs=[run(e,name,sql) for e in ['cpp','rust']]
        if error:
            passed=all(r.returncode!=0 for r in runs) and runs[0].stderr.splitlines()[:1]==runs[1].stderr.splitlines()[:1]
        else:
            passed=all(r.returncode==0 for r in runs) and list(csv.reader(io.StringIO(runs[0].stdout)))==list(csv.reader(io.StringIO(runs[1].stdout)))
        if name.startswith('unsupported-'):
            passed &= all('is not supported for Postgres binary copy' in r.stderr for r in runs)
        save(name,passed,cpp_exit=runs[0].returncode,rust_exit=runs[1].returncode)
    for name,select,types in CASES:
        paths={e:out/(name+'.'+e+'.bin') for e in ['cpp','rust']}
        writes={e:run(e,name+'-write',f'COPY ({select}) TO {literal(path)} (FORMAT postgres_binary);') for e,path in paths.items()}
        ok=all(r.returncode==0 for r in writes.values()) and all(p.exists() for p in paths.values())
        save(name+'-bytes',ok and paths['cpp'].read_bytes()==paths['rust'].read_bytes(),cpp_exit=writes['cpp'].returncode,rust_exit=writes['rust'].returncode)
        if not ok:continue
        columns='{'+','.join(f'c{i}: {literal(t)}' for i,t in enumerate(types))+'}'
        for writer,path in paths.items():
            compare(name+'-from-'+writer,f'SELECT * FROM read_postgres_binary({literal(path)},columns={columns});')
    header=b'PGCOPY\n\xff\r\n\0'+bytes(8)
    malformed={
        'empty-file': b'', 'short-header': header[:10], 'wrong-header': b'X'+header[1:]+b'\xff\xff',
        'truncated-integer': header+struct.pack('>hi',1,4)+b'\x00\x00',
        'truncated-string': header+struct.pack('>hi',1,10)+b'hello',
        'trailing-byte': header+struct.pack('>hii',1,4,42)+b'\xff\xff\x00',
    }
    for name,data in malformed.items():
        path=out/(name+'.bin');path.write_bytes(data);ty='VARCHAR' if name=='truncated-string' else 'INTEGER'
        compare(name,f'SELECT * FROM read_postgres_binary({literal(path)},columns={{i:{literal(ty)}}});',error=True)
    for name,data in [('header-only',header),('ignored-flags',header[:11]+struct.pack('>II',0x10000,1234)+struct.pack('>hii',1,4,42)+b'\xff\xff'),('missing-footer',header+struct.pack('>hii',1,4,42))]:
        path=out/(name+'.bin');path.write_bytes(data);compare(name,f'SELECT * FROM read_postgres_binary({literal(path)},columns={{i:\'INTEGER\'}});')
    path=out/'many.cpp.bin'
    for size in [0,18,19,24,37,64]:
        compare('buffer-'+str(size),f'SELECT count(*),min(i),max(i),sum(i) FROM read_postgres_binary({literal(path)},columns={{i:\'INTEGER\'}},buffer_size={size});',error=size<19)
    compare('file-uri',f'SELECT count(*) FROM read_postgres_binary({literal('file://'+str(path))},columns={{i:\'INTEGER\'}});')
    compare('prepared',f"PREPARE scan AS SELECT sum(i) FROM read_postgres_binary({literal(path)},columns={{i:'INTEGER'}},buffer_size=37); EXECUTE scan; EXECUTE scan;")
    for ty in ['UINTEGER','TINYINT','HUGEINT','TIMESTAMP_NS']:
        path=out/('unsupported-'+ty+'.bin')
        value="TIMESTAMP '2024-01-01'" if ty=='TIMESTAMP_NS' else '42'
        compare('unsupported-'+ty,f'COPY (SELECT {value}::{ty}) TO {literal(path)} (FORMAT postgres_binary);',error=True)
    path=out/'replacement.bin'
    compare('null-byte-replacement', f"SET pg_null_byte_replacement='XX'; COPY (SELECT 'a'||chr(0)||'b') TO {literal(path)} (FORMAT postgres_binary); SELECT * FROM read_postgres_binary({literal(path)},columns={{s:'VARCHAR'}});")
    compare('null-byte-rejected', f"COPY (SELECT chr(0)) TO {literal(path)} (FORMAT postgres_binary);",error=True)
    compare('null-replacement-invalid', f"SET pg_null_byte_replacement=chr(0); COPY (SELECT NULL::VARCHAR WHERE false) TO {literal(path)} (FORMAT postgres_binary);",error=True)
    path=out/'numeric-nan.bin';path.write_bytes(header+struct.pack('>hihhHH',1,8,0,0,0xc000,0)+b'\xff\xff')
    for setting in ['true','false']:
        compare('numeric-nan-'+setting, f"SET pg_numeric_nan_as_null={setting}; SELECT * FROM read_postgres_binary({literal(path)},columns={{n:'DECIMAL(6,2)'}});",error=setting=='false')
    path=out/'unsigned-read.bin';path.write_bytes(header+struct.pack('>hii',1,4,-1)+b'\xff\xff')
    compare('unsigned-read',f"SELECT * FROM read_postgres_binary({literal(path)},columns={{i:'UINTEGER'}});")
    report=dict(cases=results,sha256={name:hashlib.sha256(getattr(args,name).read_bytes()).hexdigest() for name in ['cli','oracle','extension']},scope='Raw byte comparison and both file read directions against pinned C++ DuckDB; local files only')
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not all(r['passed'] for r in results):raise SystemExit(1)


if __name__=='__main__':main()
