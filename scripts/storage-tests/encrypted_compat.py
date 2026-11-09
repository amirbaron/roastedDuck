"""Check encrypted checkpoint/WAL compatibility between authenticated native engines.

Usage: encrypted_compat.py RUST_BUNDLE CPP_BUNDLE HTTPFS_BUNDLE OUT
       --certificate-sha256 PIN

Runs four cross-engine mutation/checkpoint/original-readback journeys and eight
incomplete-tail recoveries. All directories must be on the approved artifact disk.
The caller chooses already-built bundles; pinned manifests are verified before any
native execution and again afterward. No user extension/secret directory is used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
WORKER = Path(__file__).with_name('encrypted_worker.py')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def library(bundle):
    candidates = [bundle / name for name in [
        'src/libduckdb.dylib', 'lib/libduckdb.dylib', 'lib/libduckdb.so', 'libduckdb.so']]
    found = [path for path in candidates if path.is_file()]
    if len(found) != 1:
        raise ValueError(f'Expected one native DuckDB library in {bundle}: {found}')
    return found[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['rust_bundle', 'cpp_bundle', 'httpfs_bundle', 'out']:
        parser.add_argument(name, type=Path)
    authentication = parser.add_mutually_exclusive_group(required=True)
    authentication.add_argument('--certificate-sha256')
    authentication.add_argument('--input-manifest', type=Path)
    parser.add_argument('--input-manifest-sha256')
    parser.add_argument('--cipher', choices=['GCM', 'CTR'], default='GCM')
    parser.add_argument('--encryption-version', choices=['v0', 'v1'], default='v1')
    args = parser.parse_args()
    if args.input_manifest and not args.input_manifest_sha256:
        parser.error('--input-manifest requires its independently supplied SHA256')
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    bundles = {name: getattr(args, name + '_bundle').resolve() for name in ['rust', 'cpp', 'httpfs']}
    libs = {name: library(bundles[name]) for name in ['rust', 'cpp']}
    package = bundles['httpfs'] / 'httpfs.duckdb_extension'
    env = os.environ.copy()
    env.update(TZ='UTC', TMPDIR=str(out), PYTHONDONTWRITEBYTECODE='1')

    def authenticate(label):
        if args.input_manifest:
            manifest = args.input_manifest.resolve()
            assert sha(manifest) == args.input_manifest_sha256, 'Pinned input manifest changed'
            inputs = json.loads(manifest.read_text())
            assert set(inputs['artifacts']) == {'rust', 'cpp', 'httpfs'}
            checked = {}
            for name, record in inputs['artifacts'].items():
                selected = package if name == 'httpfs' else libs[name]
                expected_path = (manifest.parent / record['path']).resolve()
                assert expected_path.is_relative_to(manifest.parent)
                assert selected.resolve() == expected_path, 'Selected runtime differs from pinned input'
                assert sha(selected) == record['sha256'], 'Pinned native input changed: ' + name
                if sys.platform == 'darwin' and name != 'httpfs':
                    subprocess.run(['codesign', '--verify', '--strict', str(selected)], check=True, capture_output=True)
                checked[name] = record
            (out / (label + '-pinned-inputs.json')).write_text(json.dumps({
                'manifest_sha256': args.input_manifest_sha256, 'checked': checked,
                'authentication': 'independently pinned build/release artifact identities',
            }, indent=2) + '\n')
            return
        for name, bundle in bundles.items():
            command = [sys.executable, '-B', str(ROOT / 'scripts/signing/verify_detached_manifest.py'),
                       str(bundle / 'SHA256SUMS'), str(bundle / 'SHA256SUMS.csig'),
                       str(bundle / 'rustdb-dev.pem'), '--tmpdir', str(out),
                       '--certificate-sha256', args.certificate_sha256]
            with (out / f'{label}-{name}-auth.log').open('x') as log:
                subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
                selected = package if name == 'httpfs' else libs[name]
                entries = dict(line.split('  ', 1)[::-1]
                               for line in (bundle / 'SHA256SUMS').read_text().splitlines() if line)
                relative = selected.relative_to(bundle).as_posix()
                assert entries.get(relative) == sha(selected), f'Native input missing from manifest: {selected}'
                if sys.platform == 'darwin' and name != 'httpfs':
                    subprocess.run(['codesign', '--verify', '--strict', str(selected)],
                                   stdout=log, stderr=subprocess.STDOUT, check=True)

    def run(case, engine, stage, mode):
        command = ['/usr/bin/nice', '-n', '10', sys.executable, '-B', str(WORKER),
                   str(case), str(libs[engine]), str(package), stage, mode,
                   args.cipher, args.encryption_version]
        with (case / f'{stage}.log').open('x') as log:
            result = subprocess.run(command, env=env, stdin=subprocess.DEVNULL,
                                    stdout=log, stderr=subprocess.STDOUT, timeout=120)
        (case / f'{stage}-terminal.json').write_text(json.dumps({'exit_code': result.returncode}) + '\n')
        if result.returncode:
            raise AssertionError((case / f'{stage}.log').read_text())
        return json.loads((case / f'{stage}-result.json').read_text())

    authenticate('before')
    journeys = []
    for producer, receiver in [('cpp', 'rust'), ('rust', 'cpp')]:
        for mode in ['checkpoint', 'wal']:
            case = out / f'{producer}-to-{receiver}-{mode}'
            case.mkdir()
            stages = []
            for stage, engine in [('create', producer), ('receive', receiver), ('readback', producer)]:
                receipt = run(case, engine, stage, mode)
                wal = case / 'encrypted.duckdb.wal'
                wal_bytes = wal.stat().st_size if wal.exists() else 0
                if stage == 'create':
                    assert wal_bytes > 0 if mode == 'wal' else wal_bytes == 0
                stages.append({'engine': engine, 'stage': stage, 'wal_bytes': wal_bytes,
                               'engine_images': receipt['engine_images']})
            journeys.append({'producer': producer, 'receiver': receiver, 'mode': mode, 'stages': stages})
            print('JOURNEY_PASS', producer, receiver, mode, flush=True)

    recoveries = []
    for producer in ['cpp', 'rust']:
        source = out / (producer + '-sealed-wal-source')
        source.mkdir()
        run(source, producer, 'create', 'wal')
        db, wal = source / 'encrypted.duckdb', source / 'encrypted.duckdb.wal'
        sealed = {'database': sha(db), 'wal': sha(wal), 'wal_bytes': wal.stat().st_size}
        assert sealed['wal_bytes'] > 1
        (source / 'sealed-inputs.json').write_text(json.dumps(sealed, indent=2) + '\n')
        for corruption in ['append_one_byte', 'truncate_one_byte']:
            receipts = {}
            inputs = {}
            for receiver in ['cpp', 'rust']:
                case = out / f'{producer}-{corruption}-read-{receiver}'
                case.mkdir()
                target = case / 'encrypted.duckdb'
                shutil.copy2(db, target)
                target_wal = case / 'encrypted.duckdb.wal'
                data = wal.read_bytes()
                data = data + bytes([1]) if corruption == 'append_one_byte' else data[:-1]
                target_wal.write_bytes(data)
                inputs[receiver] = {'database': sha(target), 'wal': sha(target_wal), 'wal_bytes': len(data)}
                (case / 'input-hashes.json').write_text(json.dumps(inputs[receiver], indent=2) + '\n')
                receipt = run(case, receiver, 'recover', corruption)
                receipts[receiver] = receipt['queries'][-1]['rows']
            assert inputs['cpp'] == inputs['rust']
            assert receipts['cpp'] == receipts['rust']
            recoveries.append({'producer': producer, 'corruption': corruption, 'inputs': inputs['cpp'],
                               'checked_rows': receipts['cpp']})
            print('RECOVERY_MATCH', producer, corruption, flush=True)
        assert sha(db) == sealed['database'] and sha(wal) == sealed['wal']

    authenticate('after')
    headers = []
    for file in sorted(out.rglob('encrypted.duckdb')):
        with file.open('rb') as stream:
            head = stream.read(124)
        assert head[8:12] == b'DUCK' and head[116] == 1, file
        assert head[118] == {'GCM': 1, 'CTR': 2}[args.cipher], file
        assert head[119] == {'v0': 0, 'v1': 1}[args.encryption_version], file
        headers.append({'file': str(file.relative_to(out)), 'metadata_hex': head[116:124].hex(),
                        'cipher_byte': head[118], 'version_byte': head[119]})
    assert len(headers) == 14, len(headers)
    verification = {'journeys_pass': 4, 'recovery_processes_pass': 8, 'native_processes': 22,
                    'cipher': args.cipher, 'encryption_version': args.encryption_version,
                    'journeys': journeys, 'recoveries': recoveries, 'physical_encrypted_headers': headers,
                    'libraries': {name: {'path': str(path), 'sha256': sha(path)} for name, path in libs.items()},
                    'scope': 'Encrypted native desktop/server checkpoint/nonempty-WAL mutation/readback and two incomplete-tail cases. Actual release/platform scope depends on supplied authenticated artifacts.'}
    (out / 'verification.json').write_text(json.dumps(verification, indent=2) + '\n')
    (out / 'terminal.json').write_text(json.dumps({'exit_code': 0}) + '\n')


if __name__ == '__main__':
    main()
