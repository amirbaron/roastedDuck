#!/usr/bin/env python3
"""Check bundled FTS/VSS loading and developer examples. Python 3.11+, no packages."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True, help='new directory for isolated data and receipts')
    args = parser.parse_args()
    cli = args.cli.resolve(strict=True)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    (out / 'tmp').mkdir()
    init = out / 'empty-init'
    init.write_text('')
    env = os.environ.copy()
    env['TMPDIR'] = str(out / 'tmp')
    report = {'passed': False, 'scope': 'developer loading smoke, not full conformance',
              'cli': str(cli), 'cli_sha256': digest(cli), 'inputs': {}, 'runs': []}

    def save():
        (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')

    setup = (f"SET extension_directory={quote(out / 'extensions')};\n"
             "SET custom_extension_repository='http://127.0.0.1:1';\n"
             "SET autoinstall_known_extensions=false;\n")

    def run(label, sql, database=':memory:', expected_error=None):
        assert digest(cli) == report['cli_sha256'], 'Selected executable changed'
        sql = setup + sql
        (out / (label + '.sql')).write_text(sql)
        proc = subprocess.run([str(cli), '-batch', '-bail', '-json', '-init', str(init), str(database)],
                              input=sql.encode(), capture_output=True, cwd=out, env=env, timeout=120)
        (out / (label + '.stdout')).write_bytes(proc.stdout)
        (out / (label + '.stderr')).write_bytes(proc.stderr)
        passed = proc.returncode == 0 if expected_error is None else (
            proc.returncode != 0 and expected_error.encode() in proc.stderr)
        report['runs'].append({'label': label, 'exit': proc.returncode, 'passed': passed,
                               'expected_error': expected_error,
                               'sql_sha256': hashlib.sha256(sql.encode()).hexdigest()})
        save()
        assert passed, f'{label}: {proc.stderr.decode(errors="replace")}'
        return proc.stdout

    try:
        examples = {}
        for name in ('fts', 'vss'):
            path = ROOT / 'extensions' / name / 'extension.toml'
            manifest = tomllib.loads(path.read_text())
            assert manifest['name'] == name and manifest['distribution'] == 'statically_linked'
            assert manifest['duckdb_version'] == '1.5.6'
            assert manifest['upstream_commit'] in (ROOT / manifest['specification']).read_text()
            for key in ('integration', 'algorithm_crate', 'specification', 'example'):
                assert (ROOT / manifest[key]).exists(), (name, key)
            report['inputs'][str(path.relative_to(ROOT))] = digest(path)
            example = ROOT / manifest['example']
            report['inputs'][manifest['example']] = digest(example)
            examples[name] = example.read_text()
            assert examples[name] in (ROOT / 'website/docs/extensions.md').read_text(), 'Guide example drift'
        check = """
SELECT CASE WHEN count(*)=2 AND bool_and(installed AND install_mode='STATICALLY_LINKED' AND NOT loaded)
THEN true ELSE error('bundled extension discovery mismatch') END AS discovered
FROM duckdb_extensions() WHERE extension_name IN ('fts','vss');
"""
        run('discover', check)
        run('load-without-install', "LOAD fts; LOAD vss; LOAD fts; LOAD vss;\n" +
            "SELECT CASE WHEN count(*)=2 AND bool_and(loaded) THEN true ELSE error('load failed') END FROM duckdb_extensions() WHERE extension_name IN ('fts','vss');")
        run('install-idempotent', 'INSTALL fts; INSTALL vss; FORCE INSTALL fts; FORCE INSTALL vss; LOAD fts; LOAD vss;')
        run('bundled-repository-error', 'INSTALL fts FROM core;', expected_error='is bundled with roastedDuck')
        run('bundled-version-error', "INSTALL vss VERSION 'other';", expected_error='is bundled with roastedDuck')
        db = out / 'documents.duckdb'
        result = run('fts-example', examples['fts'], db)
        rows = json.loads(result)
        assert len(rows) == 1 and rows[0]['id'] == 1 and rows[0]['score'] > 0, rows
        run('fts-reopen', "LOAD fts; SELECT CASE WHEN count(*)=1 THEN true ELSE error('FTS reopen failed') END FROM documents WHERE fts_main_documents.match_bm25(id,'fox') IS NOT NULL;", db)
        run('fts-refresh-drop', "LOAD fts;\nINSERT INTO documents VALUES (3,'another fox');\nPRAGMA create_fts_index('documents','id','body',overwrite=true);\nSELECT CASE WHEN count(*)=2 THEN true ELSE error('FTS refresh failed') END FROM documents WHERE fts_main_documents.match_bm25(id,'fox') IS NOT NULL;\nPRAGMA drop_fts_index('documents');", db)
        rows = json.loads(run('vss-example', examples['vss']))
        assert rows == [{'id': 1, 'distance': 0.0}], rows
        run('autoload-opt-in', "SET autoload_known_extensions=true; SELECT CASE WHEN stem('connections','english')='connect' THEN true ELSE error('stem autoload failed') END;")
        run('vss-needs-load', "SET autoload_known_extensions=true;\nSELECT * FROM pragma_hnsw_index_info();", expected_error='exists in the vss extension')
        run('vss-explicit-load', "SET autoload_known_extensions=true;\nLOAD vss;\nSELECT * FROM pragma_hnsw_index_info();")
        run('new-process-load-state', check, db)
        run('vss-persistence-boundary', 'LOAD vss; CREATE TABLE vectors(v FLOAT[3]); CREATE INDEX h ON vectors USING HNSW(v);',
            out / 'vectors.duckdb', 'HNSW indexes can only be created in in-memory databases')
        downloaded = list((out / 'extensions').rglob('*.duckdb_extension'))
        assert not downloaded, downloaded
        report['downloaded_extension_binaries'] = 0
        report['passed'] = True
        save()
        print(f"PASS: {len(report['runs'])} journeys; no downloaded extension binaries. {out / 'verification.json'}")
    except Exception as error:
        report['passed'] = False
        report['error'] = repr(error)
        save()
        raise


if __name__ == '__main__':
    main()
