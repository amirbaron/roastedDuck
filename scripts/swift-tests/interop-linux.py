"""Verify signed Linux Swift deployments and both checkpoint/WAL handoff directions."""
import argparse, datetime, json, os, shutil, subprocess, sys, time, hashlib
from pathlib import Path
workspace = Path(__file__).resolve().parents[2]
p = argparse.ArgumentParser()
p.add_argument('--cpp', type=Path, required=True)
p.add_argument('--rust', type=Path, required=True)
p.add_argument('--certificate-sha256', required=True)
p.add_argument('--out', type=Path, required=True)
a = p.parse_args()
out = a.out.resolve()
out.mkdir()
(out / 'tmp').mkdir()
roots = {'cpp': a.cpp.resolve(), 'rust': a.rust.resolve()}
deployed = {}
env = dict(os.environ, TMPDIR=str(out / 'tmp'), TZ='UTC')
env.pop('LD_LIBRARY_PATH', None)

def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()

def auth(root, label):
    command = [sys.executable, str(workspace / 'scripts/signing/verify_detached_manifest.py'), str(root / 'SHA256SUMS'), str(root / 'SHA256SUMS.csig'), str(root / 'rustdb-dev.pem'), '--tmpdir', str(out / 'tmp'), '--certificate-sha256', a.certificate_sha256]
    with (out / (label + '-signature.log')).open('x') as log:
        subprocess.run(command, check=True, env=env, stdout=log, stderr=subprocess.STDOUT)
for engine, root in roots.items():
    auth(root, engine + '-original-before')
    dest = out / 'deployed' / engine
    shutil.copytree(root, dest)
    auth(dest, engine + '-deployed-before')
    deployed[engine] = dest

def identity(engine, path):
    m = json.loads((path / 'identity.json').read_text())
    expected = str(deployed[engine] / 'lib/libduckdb.so')
    assert m['function_image'] == expected and m['engine_images'] == [expected] and (m['version'] == 'v1.5.6'), m
    return m

def command(engine, path, mode):
    return ['nice', '-n', '10', str(deployed[engine] / 'Consumer'), str(path), mode]

def run(engine, path, mode, case, label):
    with (case / (label + '.log')).open('x') as log:
        r = subprocess.run(command(engine, path, mode), env=env, stdout=log, stderr=subprocess.STDOUT, timeout=60)
    (case / (label + '-terminal.json')).write_text(json.dumps({'command': command(engine, path, mode), 'exit_code': r.returncode}) + '\n')
    assert r.returncode == 0, (case / (label + '.log')).read_text()[-3500:]
    m = json.loads((path / 'consumer.json').read_text())
    identity(engine, path)
    assert m['function_image'] == str(deployed[engine] / 'lib/libduckdb.so')
    for name in ['decimal', 'blob', 'timestamp', 'list-null', 'struct-unicode']:
        assert name in m['checks']
    shutil.copy2(path / 'consumer.json', case / (label + '.json'))
    (path / 'consumer.json').unlink()
    return m
normal = []
for engine in roots:
    case = out / ('normal-' + engine)
    case.mkdir()
    data = case / 'data'
    data.mkdir()
    m = run(engine, data, 'normal', case, 'run')
    assert m['ids'] == [1, 2, 3] and m['labels'] == ['prépared שלום', 'appended', None]
    normal.append({'engine': engine, 'report': m})
    print(engine + ' deployed Swift normal PASS', flush=True)
cases = []
for writer, reader in [('cpp', 'rust'), ('rust', 'cpp')]:
    for kind in ['checkpoint', 'wal']:
        name = writer + '-' + kind
        case = out / name
        case.mkdir()
        data = case / 'writer'
        data.mkdir()
        cmd = command(writer, data, kind + '-writer')
        snap = case / 'before-replay'
        snap.mkdir()
        with (case / 'writer.log').open('x') as log:
            proc = subprocess.Popen(cmd, env=env, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)
            try:
                if kind == 'wal':
                    deadline = time.monotonic() + 45
                    while not (data / 'writer-ready').exists():
                        assert proc.poll() is None, ('writer exited', proc.returncode, (case / 'writer.log').read_text()[-3000:])
                        assert time.monotonic() < deadline, 'WAL readiness timeout'
                        time.sleep(0.05)
                else:
                    assert proc.wait(timeout=60) == 0, (case / 'writer.log').read_text()[-3000:]
                writer_identity = identity(writer, data)
                shutil.copy2(data / 'identity.json', case / 'writer-identity.json')
                files = {}
                for f in data.glob('client.duckdb*'):
                    shutil.copy2(f, snap / f.name)
                    files[f.name] = {'bytes': f.stat().st_size, 'sha256': sha(f)}
                if kind == 'wal':
                    assert files['client.duckdb.wal']['bytes'] > 0
            finally:
                if proc.poll() is None:
                    proc.kill()
                proc.wait(timeout=10)
                proc.stdin.close()
        (case / 'writer-terminal.json').write_text(json.dumps({'command': cmd, 'exit_code': proc.returncode, 'intentional_wal_sigkill': kind == 'wal'}) + '\n')
        assert proc.returncode == (-9 if kind == 'wal' else 0)
        active = case / 'handoff'
        shutil.copytree(snap, active)
        reports = []
        for engine, mode in [(reader, 'read-write'), (writer, 'read-final')]:
            report = run(engine, active, mode, case, engine + '-' + mode)
            assert report['ids'] == [1, 2, 3, 4, 5] and report['labels'] == ['prépared שלום', 'appended', None, 'wal', 'handoff']
            reports.append(report)
        for engine in roots:
            auth(deployed[engine], engine + '-after-' + name)
        cases.append({'writer': writer, 'reader': reader, 'kind': kind, 'snapshots': files, 'writer_identity': writer_identity, 'writer_exit_code': proc.returncode, 'reports': reports})
        (out / 'progress.json').write_text(json.dumps(cases, indent=2) + '\n')
        print(name + ' Swift typed handoff PASS', flush=True)
for engine, root in roots.items():
    auth(root, engine + '-original-after')
    auth(deployed[engine], engine + '-deployed-after')
report = {'passed_linux_swift_consumer_gate': True, 'normal_runs': normal, 'journeys': 4, 'cases': cases, 'relocated_runtime_identity_verified': True, 'signed_package_and_payloads_unchanged': True, 'scope': 'Linux arm64 pinned Swift independent local package plus executable/library deployment, prepared/Appender/PK error/rollback/NULL/Unicode and DECIMAL/BLOB/TIMESTAMP/list/struct checkpoint and nonemptyWAL both directions, receiver mutation/checkpoint and origin readback. Other platforms/final gates open.', 'utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}
(out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps({'passed': True, 'journeys': 4}), flush=True)
