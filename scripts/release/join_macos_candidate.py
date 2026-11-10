#!/usr/bin/env python3
"""Join authenticated same-run Mac native and wheel builds without recompiling."""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

from archive_native_sdk import verify_archive
from observer_protocol import api, download_artifact, sha

ROOT = Path(__file__).resolve().parents[2]
REPOSITORY = 'amirbaron/roastedDuck'
COMPONENTS = ('candidate-native-bundle-macos-arm64', 'candidate-wheels-macos-arm64',
              'candidate-native-manifest-macos-arm64')


def verify_sources(native, wheels, commit, run_id):
    assert native.read_bytes() == wheels.read_bytes(), 'Native and wheel source receipts differ'
    record = json.loads(native.read_text())
    assert record['commit'] == commit
    assert record['github_run_id'] == str(run_id)
    assert record['github_repository'] == REPOSITORY
    assert record['cargo_build_jobs'] == '1'
    assert record['cargo_profile'] == 'release (repository-defined; no LTO or optimization overrides)'
    assert record['source_sha256'] and record['tree']
    return record


def verify_wheel_names(directory, version):
    expected = {f'roastedduck-{version}-cp31{i}-cp31{i}-macosx_11_0_arm64.whl' for i in range(5)}
    assert {p.name for p in directory.iterdir()} == expected, 'Expected all five normal Mac ABI wheels'


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--run', type=int, required=True)
    p.add_argument('--commit', required=True)
    p.add_argument('--out', type=Path, required=True)
    a = p.parse_args()
    assert re.fullmatch(r'[0-9a-f]{40}', a.commit)
    assert subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() == a.commit
    run = api(f'repos/{REPOSITORY}/actions/runs/{a.run}')
    assert run['head_sha'] == a.commit and run['head_repository']['full_name'] == REPOSITORY
    assert run['path'].split('@')[0] == '.github/workflows/build-candidate.yml'
    jobs = api(f'repos/{REPOSITORY}/actions/runs/{a.run}/jobs?per_page=100')['jobs']
    for name in ('mac-native', 'build (macos-26-intel, macos-26, macos-arm64, aarch64-apple-darwin, off)'):
        matches = [j for j in jobs if j['name'] == name]
        assert len(matches) == 1 and matches[0]['status'] == 'completed' and matches[0]['conclusion'] == 'success', name
    catalog = api(f'repos/{REPOSITORY}/actions/runs/{a.run}/artifacts?per_page=100')['artifacts']
    a.out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='mac-components-', dir=a.out.parent) as temp:
        temp = Path(temp)
        transport = []
        for name in COMPONENTS:
            matches = [r for r in catalog if r['name'] == name]
            assert len(matches) == 1, name
            record = api(f'repos/{REPOSITORY}/actions/artifacts/{matches[0]["id"]}')
            assert record['digest'] == matches[0]['digest']
            transport.append(download_artifact(REPOSITORY, record, a.run, a.commit, temp / name, temp))
        native, wheels, manifest = (temp / name for name in COMPONENTS)
        source = verify_sources(native / 'receipts/build-inputs.json', wheels / 'receipts/build-inputs.json', a.commit, a.run)
        verify_wheel_names(wheels / 'wheelhouse', source['release']['version'])
        subprocess.run([sys.executable, str(ROOT / 'scripts/release/inspect_distribution.py'), str(wheels / 'wheelhouse'),
                        '--report', str(temp / 'wheel-inspection.json')], check=True)
        assert json.loads((temp / 'wheel-inspection.json').read_text()) == json.loads((wheels / 'receipts/distributions.json').read_text())
        unpacked = temp / 'unpacked'
        unpacked.mkdir()
        with tarfile.open(native / 'native-macos-arm64.tar.gz') as archive:
            archive.extractall(unpacked, filter='data')
        assert (unpacked / 'native/build-provenance.json').read_bytes() == (manifest / 'build-provenance.json').read_bytes()
        subprocess.run([sys.executable, str(ROOT / 'scripts/release/verify_native_inputs.py'), '--native', str(unpacked / 'native'),
                        '--build-inputs', str(native / 'receipts/build-inputs.json'), '--expected-commit', a.commit,
                        '--out', str(temp / 'native-verification.json')], check=True)
        provenance = json.loads((manifest / 'build-provenance.json').read_text())
        sdk = native / f"roastedduck-{source['release']['version']}-macos-arm64.tar.gz"
        assert provenance['sdk_archive']['filename'] == sdk.name
        sdk_proof = verify_archive(sdk, provenance)
        # The public candidate contains the original component bytes, not repacked wheels or libraries.
        shutil.copytree(wheels, a.out)
        for path in (sdk, native / 'native-macos-arm64.tar.gz'):
            assert not (a.out / path.name).exists()
            shutil.copy2(path, a.out / path.name)
        receipt = {'passed': True, 'source_commit': a.commit, 'run_id': a.run,
                   'components': transport, 'build_inputs_sha256': sha(native / 'receipts/build-inputs.json'),
                   'native_manifest_sha256': sha(manifest / 'build-provenance.json'), 'sdk_verification': sdk_proof,
                   'native_verification': json.loads((temp / 'native-verification.json').read_text()),
                   'wheel_inspection_sha256': sha(temp / 'wheel-inspection.json')}
        (a.out / 'receipts/mac-component-verification.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print('Verified and joined same-run native SDK and all five Mac ABI wheels')


if __name__ == '__main__':
    main()
