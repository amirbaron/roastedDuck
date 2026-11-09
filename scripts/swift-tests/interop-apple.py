"""Exercise signed deployed Mac Swift consumers in both persistence directions."""
import argparse
import datetime
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import time

from package_identity import digest, symlinks, verify_manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cpp', required=True, type=Path)
    parser.add_argument('--rust', required=True, type=Path)
    parser.add_argument('--certificate-sha256', required=True)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    if os.getpriority(os.PRIO_PROCESS, 0) < 10:
        os.setpriority(os.PRIO_PROCESS, 0, 10)
    inherited_priority = os.getpriority(os.PRIO_PROCESS, 0)
    assert inherited_priority >= 10
    out = args.out.resolve()
    out.mkdir()
    temporary = out / 'tmp'
    temporary.mkdir()
    home = out / 'foundation-home'
    home.mkdir()
    env = dict(os.environ, TMPDIR=str(temporary), CFFIXED_USER_HOME=str(home), TZ='UTC')
    for name in ['DYLD_LIBRARY_PATH', 'DYLD_FRAMEWORK_PATH', 'DYLD_INSERT_LIBRARIES']:
        env.pop(name, None)
    originals = {'cpp': args.cpp.resolve(), 'rust': args.rust.resolve()}
    deployed = {}
    images = {}
    versions = {}
    links = {}
    architecture = None

    def authenticate(root):
        verify_manifest(root, args.certificate_sha256, temporary)
        for path in root.rglob('*'):
            if path.is_symlink():
                assert path.resolve().is_relative_to(root), 'deployment link escapes root'
        subprocess.run(['codesign', '--verify', '--strict', str(root / 'Consumer')], check=True)

    for engine, root in originals.items():
        authenticate(root)
        links[engine] = symlinks(root)
        dest = out / 'deployed' / engine
        shutil.copytree(root, dest, symlinks=True)
        authenticate(dest)
        assert symlinks(dest) == links[engine]
        actual_arch = subprocess.check_output(['lipo', '-archs', str(dest / 'Consumer')], text=True).strip()
        assert actual_arch in ['arm64', 'x86_64']
        architecture = architecture or actual_arch
        assert architecture == actual_arch, 'consumer architectures differ'
        relative = 'lib/libduckdb.dylib' if engine == 'cpp' else 'Cduckdb.framework/Versions/A/Cduckdb'
        image = (dest / relative).resolve()
        assert image.is_relative_to(dest)
        subprocess.run(['codesign', '--verify', '--strict', str(image)], check=True)
        deployed[engine], images[engine] = dest, image

    def identity(engine, data):
        result = json.loads((data / 'identity.json').read_text())
        assert result['function_image'] == str(images[engine])
        assert result['engine_images'] == [str(images[engine])]
        assert result['version']
        if engine == 'rust':
            assert result['version'] == 'v1.5.6'
        if engine in versions:
            assert result['version'] == versions[engine]
        else:
            versions[engine] = result['version']
        return result

    def command(engine, data, mode):
        return [str(deployed[engine] / 'Consumer'), str(data), mode]

    def run(engine, data, mode, case, label):
        cmd = command(engine, data, mode)
        with (case / (label + '.log')).open('x') as log:
            result = subprocess.run(cmd, env=env, stdout=log, stderr=subprocess.STDOUT,
                                    timeout=60)
        (case / (label + '-terminal.json')).write_text(json.dumps({'command': cmd, 'exit_code': result.returncode}) + '\n')
        assert result.returncode == 0, (case / (label + '.log')).read_text()[-3000:]
        identity(engine, data)
        report = json.loads((data / 'consumer.json').read_text())
        assert report['function_image'] == str(images[engine])
        for check in ['decimal', 'blob', 'timestamp', 'list-null', 'struct-unicode']:
            assert check in report['checks']
        shutil.copy2(data / 'consumer.json', case / (label + '.json'))
        (data / 'consumer.json').unlink()
        return report

    normal = []
    for engine in originals:
        case = out / ('normal-' + engine)
        data = case / 'data'
        data.mkdir(parents=True)
        report = run(engine, data, 'normal', case, 'run')
        assert report['ids'] == [1, 2, 3]
        assert report['labels'] == ['prépared שלום', 'appended', None]
        normal.append({'engine': engine, 'report': report})

    cases = []
    for writer, reader in [('cpp', 'rust'), ('rust', 'cpp')]:
        for kind in ['checkpoint', 'wal']:
            name = writer + '-' + kind
            case = out / name
            data = case / 'writer'
            data.mkdir(parents=True)
            cmd = command(writer, data, kind + '-writer')
            snapshot = case / 'before-replay'
            snapshot.mkdir()
            with (case / 'writer.log').open('x') as log:
                process = subprocess.Popen(cmd, env=env, stdin=subprocess.PIPE, stdout=log,
                                           stderr=subprocess.STDOUT)
                try:
                    if kind == 'wal':
                        deadline = time.monotonic() + 45
                        while not (data / 'writer-ready').exists():
                            assert process.poll() is None, 'WAL writer exited before readiness'
                            assert time.monotonic() < deadline, 'WAL readiness deadline'
                            time.sleep(0.05)
                    else:
                        assert process.wait(timeout=60) == 0
                    writer_identity = identity(writer, data)
                    shutil.copy2(data / 'identity.json', case / 'writer-identity.json')
                    files = {}
                    for file in data.glob('client.duckdb*'):
                        if file.is_file():
                            shutil.copy2(file, snapshot / file.name)
                            files[file.name] = {'bytes': file.stat().st_size, 'sha256': digest(file)}
                    assert 'client.duckdb' in files
                    if kind == 'wal':
                        assert files['client.duckdb.wal']['bytes'] > 0
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=10)
                    process.stdin.close()
            assert process.returncode == (-9 if kind == 'wal' else 0)
            (case / 'writer-terminal.json').write_text(json.dumps({'command': cmd,
                'exit_code': process.returncode, 'intentional_wal_sigkill': kind == 'wal'}) + '\n')
            active = case / 'handoff'
            shutil.copytree(snapshot, active)
            reports = []
            for engine, mode in [(reader, 'read-write'), (writer, 'read-final')]:
                report = run(engine, active, mode, case, engine + '-' + mode)
                assert report['ids'] == [1, 2, 3, 4, 5]
                assert report['labels'] == ['prépared שלום', 'appended', None, 'wal', 'handoff']
                reports.append(report)
            for engine, root in deployed.items():
                authenticate(root)
                assert symlinks(root) == links[engine]
            cases.append({'writer': writer, 'reader': reader, 'kind': kind, 'snapshots': files,
                          'writer_identity': writer_identity, 'writer_exit_code': process.returncode,
                          'reports': reports})
            (out / 'progress.json').write_text(json.dumps(cases, indent=2) + '\n')
            print(name + ' Swift typed journey PASS', flush=True)

    for engine, root in originals.items():
        authenticate(root)
        authenticate(deployed[engine])
        assert symlinks(root) == links[engine]
    report = {'passed_apple_swift_typed_persistence_gate': True, 'architecture': architecture,
              'inherited_nice': inherited_priority,
              'versions': versions, 'normal_runs': normal, 'journeys': 4, 'cases': cases,
              'native_images': {engine: {'path': str(image), 'sha256': digest(image)} for engine, image in images.items()},
              'signed_payloads_and_links_unchanged': True,
              'execution': 'native arm64' if architecture == platform.machine() else 'Intel under Rosetta on arm64 host',
              'scope': 'Independent deployed Mac Swift consumers, both checkpoint/nonempty WAL directions, typed values, receiver mutation/checkpoint and original readback. Not iOS or performance acceptance.',
              'utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
