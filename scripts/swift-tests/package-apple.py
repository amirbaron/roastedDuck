"""Package authenticated macOS/iOS slices for the pinned Swift client.

The three arm64 Apple slices are required; Intel slices are optional. Packaging never executes an engine,
and cannot establish simulator/device runtime or Apple distribution approval.
"""

import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import struct
import subprocess
import sys

from prepare import digest


SLICES = {
    'macos-arm64': ('macos', 'arm64', 1),
    'macos-x86_64': ('macos', 'x86_64', 1),
    'ios-arm64': ('ios', 'arm64', 2),
    'ios-simulator-arm64': ('ios-simulator', 'arm64', 7),
    'ios-simulator-x86_64': ('ios-simulator', 'x86_64', 7),
}
REQUIRED_SLICES = {'macos-arm64', 'ios-arm64', 'ios-simulator-arm64'}


def version(number):
    return '.'.join(str(part) for part in
                    (number >> 16, (number >> 8) & 255, number & 255))


def image_info(path):
    """Read the little-endian thin 64-bit dylib header and build command."""
    with path.open('rb') as stream:
        header = stream.read(32)
        assert len(header) == 32, 'truncated Mach-O header'
        magic, cpu, _, kind, count, size, _, _ = struct.unpack('<8I', header)
        assert magic == 0xFEEDFACF and kind == 6, 'thin 64-bit dylib required'
        assert size <= 1024 * 1024 and count <= size // 8, 'invalid load commands'
        commands = stream.read(size)
    assert len(commands) == size, 'truncated load commands'
    builds = []
    offset = 0
    for _ in range(count):
        assert offset + 8 <= size, 'truncated command'
        command, length = struct.unpack_from('<2I', commands, offset)
        assert length >= 8 and offset + length <= size, 'invalid command length'
        if command == 0x32:  # LC_BUILD_VERSION
            assert length >= 24
            platform, minimum, sdk = struct.unpack_from('<3I', commands, offset + 8)
            builds.append({'platform': platform, 'minimum': version(minimum),
                           'sdk': version(sdk)})
        offset += length
    assert offset == size and len(builds) == 1, 'one build-version command required'
    architecture = {0x0100000C: 'arm64', 0x01000007: 'x86_64'}[cpu]
    return {'architecture': architecture, **builds[0]}


def authenticate(root, pin, temporary):
    signing = Path(__file__).resolve().parents[1] / 'signing'
    sys.path.insert(0, str(signing))
    from verify_detached_manifest import verify
    verify(root / 'SHA256SUMS', root / 'SHA256SUMS.csig',
           root / 'rustdb-dev.pem', 'openssl', temporary, pin)
    entries = {line[66:]: line[:64] for line in (root / 'SHA256SUMS').read_text().splitlines()}
    library = root / 'lib/libduckdb.dylib'
    assert entries.get('lib/libduckdb.dylib') == digest(library), 'image not in signed manifest'
    subprocess.run(['codesign', '--verify', '--strict', str(library)], check=True)
    return library


def sign(path, identity):
    subprocess.run(['codesign', '--force', '--sign', identity, '--timestamp=none',
                    '--identifier', 'dev.rustdb.Cduckdb', str(path)], check=True)
    subprocess.run(['codesign', '--verify', '--strict', str(path)], check=True)


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True, type=Path)
    parser.add_argument('--slice', action='append', required=True, metavar='NAME=SIGNED_ROOT')
    parser.add_argument('--certificate-sha256', required=True)
    parser.add_argument('--identity', default='rustdb-dev')
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--check-only', action='store_true',
                        help='authenticate and inspect inputs, without creating a package')
    args = parser.parse_args()
    roots = {}
    for value in args.slice:
        name, separator, root = value.partition('=')
        if not separator or name not in SLICES or name in roots:
            parser.error('each named slice must occur exactly once: ' + ', '.join(SLICES))
        roots[name] = Path(root).resolve()
    if not REQUIRED_SLICES.issubset(roots):
        parser.error('all three arm64 Apple slices are required: ' + ', '.join(sorted(REQUIRED_SLICES)))
    if not re.fullmatch(r'[0-9a-fA-F]{64}', args.certificate_sha256):
        parser.error('a trusted SHA256 certificate fingerprint is required')
    out = args.out.resolve()
    if out.exists():
        parser.error('preserve evidence: output must be new')
    pin = json.loads((workspace / 'swift/upstream.json').read_text())
    source = args.source.resolve()
    client = source / 'tools/swift/duckdb-swift'
    header = source / 'src/include/duckdb.h'
    assert digest(header) == pin['header_sha256']
    for item in pin['files']:
        assert digest(client / item['path']) == item['sha256'], item['path']
    out.mkdir(parents=True)
    temporary = out / 'tmp'
    temporary.mkdir()
    records = {}
    exports = None
    for name, root in roots.items():
        library = authenticate(root, args.certificate_sha256, temporary)
        info = image_info(library)
        group, architecture, platform = SLICES[name]
        assert info['architecture'] == architecture and info['platform'] == platform, (name, info)
        symbols = subprocess.check_output(['nm', '-gjU', str(library)], text=True).splitlines()
        duckdb_symbols = sorted(symbol for symbol in symbols if symbol.startswith('_duckdb_'))
        assert len(duckdb_symbols) == 547, (name, len(duckdb_symbols))
        assert not any('adbc' in symbol.lower() and 'test' in symbol.lower()
                       for symbol in symbols), 'test bridge in shipping slice'
        if exports is None:
            exports = duckdb_symbols
        assert duckdb_symbols == exports, 'C API exports differ between slices'
        records[name] = {'root': str(root), 'library': str(library), 'sha256': digest(library),
                         'input_manifest_sha256': digest(root / 'SHA256SUMS'),
                         'group': group, **info}
    report = {'slices': records, 'c_api_exports_each_slice': exports,
              'certificate_sha256': args.certificate_sha256,
              'source_pin_sha256': digest(workspace / 'swift/upstream.json'),
              'runtime_verified': False, 'apple_distribution_approved': False,
              'scope': 'Authenticated Apple input preflight only; Intel slices are optional'}
    (out / 'preflight.json').write_text(json.dumps(report, indent=2) + '\n')
    if args.check_only:
        print(json.dumps({'preflight_passed': True, 'slice_count': len(records), 'runtime_verified': False}))
        return

    package = out / 'DuckDBRust'
    package.mkdir()
    for item in pin['files']:
        if item['path'].startswith('Tests/'):
            continue
        target = package / item['path']
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(client / item['path'], target)
    frameworks = []
    minimums = {}
    env = dict(os.environ, TMPDIR=str(temporary))
    for group in ['macos', 'ios', 'ios-simulator']:
        selected = [r for r in records.values() if r['group'] == group]
        versions = {r['minimum'] for r in selected}
        assert len(versions) == 1, 'deployment minimum differs within a universal slice'
        minimum = versions.pop()
        minimums[group] = minimum
        framework = out / 'frameworks' / group / 'Cduckdb.framework'
        content = framework / 'Versions/A' if group == 'macos' else framework
        for name in ['Headers', 'Modules']:
            (content / name).mkdir(parents=True, exist_ok=True)
        binary = content / 'Cduckdb'
        subprocess.run(['lipo', '-create', *[r['library'] for r in selected],
                        '-output', str(binary)], env=env, check=True)
        shutil.copy2(header, content / 'Headers/duckdb.h')
        (content / 'Modules/module.modulemap').write_text(
            'framework module Cduckdb {\n  umbrella header "duckdb.h"\n  export *\n}\n')
        metadata = {'CFBundleExecutable': 'Cduckdb', 'CFBundleIdentifier': 'dev.rustdb.Cduckdb',
                    'CFBundleName': 'Cduckdb', 'CFBundlePackageType': 'FMWK',
                    'CFBundleShortVersionString': pin['version'], 'CFBundleVersion': pin['version'],
                    'CFBundleSupportedPlatforms': [{'macos': 'MacOSX', 'ios': 'iPhoneOS',
                                                   'ios-simulator': 'iPhoneSimulator'}[group]],
                    'MinimumOSVersion': minimum}
        if group == 'macos':
            resources = content / 'Resources'
            resources.mkdir()
            (resources / 'Info.plist').write_bytes(plistlib.dumps(metadata))
            (framework / 'Versions/Current').symlink_to('A')
            for name in ['Cduckdb', 'Headers', 'Modules', 'Resources']:
                (framework / name).symlink_to('Versions/Current/' + name)
        else:
            (content / 'Info.plist').write_bytes(plistlib.dumps(metadata))
        subprocess.run(['install_name_tool', '-id', '@rpath/Cduckdb.framework/Cduckdb',
                        str(binary)], env=env, check=True)
        sign(framework, args.identity)
        frameworks.append(framework)
    assert minimums['ios'] == minimums['ios-simulator'], 'device/simulator minima differ'
    artifacts = package / 'Artifacts'
    artifacts.mkdir()
    xcframework = artifacts / 'Cduckdb.xcframework'
    command = ['nice', '-n', '10', 'xcodebuild', '-create-xcframework']
    for framework in frameworks:
        command += ['-framework', str(framework)]
    command += ['-output', str(xcframework)]
    with (out / 'xcframework.log').open('x') as log:
        subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    index = plistlib.loads((xcframework / 'Info.plist').read_bytes())
    expected = {}
    for name in roots:
        group, architecture, _ = SLICES[name]
        key = ('macos', '') if group == 'macos' else ('ios', 'simulator' if group == 'ios-simulator' else '')
        expected.setdefault(key, set()).add(architecture)
    actual = {}
    for entry in index['AvailableLibraries']:
        key = (entry['SupportedPlatform'], entry.get('SupportedPlatformVariant', ''))
        assert key not in actual, 'duplicate XCFramework platform'
        actual[key] = set(entry['SupportedArchitectures'])
        bundled = (xcframework / entry['LibraryIdentifier'] / entry['LibraryPath']).resolve()
        assert bundled.is_relative_to(xcframework), 'framework outside package'
        subprocess.run(['codesign', '--verify', '--strict', str(bundled)], check=True)
        binary = bundled / 'Cduckdb'
        architectures = subprocess.check_output(['lipo', '-archs', str(binary)], text=True).split()
        assert set(architectures) == actual[key], 'packaged binary architecture mismatch'
    assert actual == expected, 'XCFramework platform/architecture matrix mismatch'
    sign(xcframework, args.identity)
    (package / 'Package.swift').write_text('''// swift-tools-version: 5.7
import PackageDescription
let package = Package(
    name: "DuckDBRust",
    platforms: [.macOS("MAC_MIN"), .iOS("IOS_MIN")],
    products: [.library(name: "DuckDB", targets: ["DuckDB"])],
    targets: [
        .binaryTarget(name: "Cduckdb", path: "Artifacts/Cduckdb.xcframework"),
        .target(name: "DuckDB", dependencies: ["Cduckdb"])
    ]
)
'''.replace('MAC_MIN', minimums['macos']).replace('IOS_MIN', minimums['ios']))
    for record in records.values():
        library = authenticate(Path(record['root']), args.certificate_sha256, temporary)
        assert digest(library) == record['sha256'], 'original slice changed'
    report.update({'package': str(package), 'minimum_macos': minimums['macos'],
                   'minimum_ios': minimums['ios'],
                   'files': {str(p.relative_to(package)): digest(p) for p in sorted(package.rglob('*'))
                             if p.is_file() and not p.is_symlink()},
                   'scope': 'Signed Apple Swift package; consumer/device/simulator runtime is a separate gate'})
    (out / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'package': str(package), 'slice_count': len(records), 'runtime_verified': False}))


if __name__ == '__main__':
    main()
