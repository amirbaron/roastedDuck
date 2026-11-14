#!/usr/bin/env python3
"""Install development-only emulators into this workspace's artifact storage."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys

REPO = Path(__file__).resolve().parents[2]


def artifact_root():
    if os.environ.get('CONDUCTOR_IS_LOCAL') == '1':
        mount = '/Volumes/amirs ssd'
        info = plistlib.loads(subprocess.check_output(['diskutil', 'info', '-plist', mount]))
        if info.get('MountPoint') != mount:
            raise SystemExit('External SSD is not mounted; no internal-disk fallback.')
        workspace = Path(os.environ['CONDUCTOR_WORKSPACE_PATH']).resolve()
        repo_name = Path(os.environ['CONDUCTOR_ROOT_PATH']).name
        key = hashlib.sha256(str(workspace).encode()).hexdigest()[:16]
        artifacts = Path(mount) / 'ai context/conductor' / repo_name / f'{workspace.name}-{key}'
        artifacts.mkdir(parents=True, exist_ok=True)
        link = workspace / '.context/artifacts'
        link.parent.mkdir(exist_ok=True)
        if not link.exists() and not link.is_symlink():
            link.symlink_to(artifacts)
        elif link.resolve() != artifacts:
            print(f'Artifact link conflict; using {artifacts} directly.')
        return artifacts / 'local-services'
    return (REPO / '.context/artifacts/local-services').resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    root = artifact_root()
    root.mkdir(parents=True, exist_ok=True)
    if (root / 'active').exists():
        raise SystemExit('Local fixture run is active; stop it before changing its tools.')
    for name in ['tmp', 'pip-cache', 'npm-cache', 'node']:
        (root / name).mkdir(exist_ok=True)
    env = dict(os.environ, TMPDIR=str(root / 'tmp'), PIP_CACHE_DIR=str(root / 'pip-cache'))
    python = root / 'venv/bin/python'
    if not python.exists():
        subprocess.run([sys.executable, '-m', 'venv', str(root / 'venv')], check=True, env=env)
    subprocess.run([str(python), '-m', 'pip', 'install', '-r',
                    str(REPO / 'scripts/local-services/requirements.txt')], check=True, env=env)
    for name in ['package.json', 'package-lock.json']:
        source = REPO / 'scripts/local-services' / name
        if source.exists():
            shutil.copyfile(source, root / 'node' / name)
    subprocess.run(['npm', '--cache', str(root / 'npm-cache'), '--prefix', str(root / 'node'),
                    'ci' if (root / 'node/package-lock.json').exists() else 'install',
                    '--no-audit', '--no-fund'], check=True, env=env)
    freeze = subprocess.check_output([str(python), '-m', 'pip', 'freeze'], env=env, text=True)
    (root / 'python-versions.txt').write_text(freeze)
    (root / 'tools.json').write_text(json.dumps({'root': str(root), 'python': str(python)}, indent=2) + '\n')
    print(f'Fixture tools installed at {root}')
    print('These tools are development fixtures, not shipped engine dependencies.')


if __name__ == '__main__':
    main()
