"""Authenticate a complete local Swift SDK, including sources and symlink layout."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def package_files(package):
    return {str(p.relative_to(package)): digest(p) for p in sorted(package.rglob('*'))
            if p.is_file() and not p.is_symlink()}


def symlinks(package):
    result = {}
    for path in sorted(package.rglob('*')):
        if path.is_symlink():
            assert path.resolve().is_relative_to(package), 'SDK symlink escapes package'
            result[str(path.relative_to(package))] = os.readlink(path)
    return result


def verify_manifest(root, certificate_sha256, temporary):
    temporary.mkdir(parents=True, exist_ok=True)
    signing = Path(__file__).resolve().parents[1] / 'signing'
    sys.path.insert(0, str(signing))
    from verify_detached_manifest import verify
    return verify(root / 'SHA256SUMS', root / 'SHA256SUMS.csig', root / 'rustdb-dev.pem',
                  'openssl', temporary, certificate_sha256)


def verify_sdk(sdk, certificate_sha256, temporary):
    count = verify_manifest(sdk, certificate_sha256, temporary)
    package = sdk / 'DuckDBRust'
    expected = json.loads((sdk / 'summary.json').read_text())
    assert package_files(package) == expected['files'], 'SDK payload set or hashes changed'
    assert symlinks(package) == json.loads((sdk / 'sdk-symlinks.json').read_text()), 'SDK symlink layout changed'
    subprocess.run(['codesign', '--verify', '--strict', '--deep',
                    str(package / 'Artifacts/Cduckdb.xcframework')], check=True)
    return count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sdk', required=True, type=Path)
    parser.add_argument('--certificate', required=True, type=Path)
    parser.add_argument('--certificate-sha256', required=True)
    parser.add_argument('--identity', default='rustdb-dev')
    parser.add_argument('--tmpdir', required=True, type=Path)
    args = parser.parse_args()
    sdk = args.sdk.resolve()
    package = sdk / 'DuckDBRust'
    for name in ['SHA256SUMS', 'SHA256SUMS.csig', 'rustdb-dev.pem', 'sdk-symlinks.json']:
        assert not (sdk / name).exists(), 'Preserve existing SDK authentication: ' + name
    expected = json.loads((sdk / 'summary.json').read_text())
    assert package_files(package) == expected['files'], 'SDK differs from packaging receipt'
    shutil.copy2(args.certificate, sdk / 'rustdb-dev.pem')
    (sdk / 'sdk-symlinks.json').write_text(json.dumps(symlinks(package), indent=2) + '\n')
    payloads = [package / name for name in expected['files']]
    payloads += [sdk / name for name in ['summary.json', 'preflight.json', 'rustdb-dev.pem', 'sdk-symlinks.json']]
    manifest = sdk / 'SHA256SUMS'
    manifest.write_text(''.join(digest(p) + '  ' + p.relative_to(sdk).as_posix() + '\n'
                                for p in sorted(payloads)))
    subprocess.run(['codesign', '--force', '--sign', args.identity, '--timestamp=none',
                    '--identifier', 'dev.rustdb.AppleSwiftSdkManifest', '--detached',
                    str(sdk / 'SHA256SUMS.csig'), str(manifest)], check=True)
    count = verify_sdk(sdk, args.certificate_sha256, args.tmpdir)
    print(json.dumps({'authenticated_payloads': count, 'package_files': len(expected['files']),
                      'runtime_verified_by_signing': False}))


if __name__ == '__main__':
    main()
