"""Relocate a binary package and build/sign/run a separate Swift client."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess

from prepare import digest
from package_identity import verify_manifest, verify_sdk


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--triple", help="explicit macOS or iOS device/simulator target")
    parser.add_argument("--apple-sdk", type=Path, help="compiler SDK path, required for iOS")
    parser.add_argument("--build-only", action="store_true", help="sign and verify the consumer without executing it")
    parser.add_argument("--certificate-sha256", help="trusted complete SDK manifest certificate fingerprint")
    args = parser.parse_args()
    sdk, out = args.sdk.resolve(), args.out.resolve()
    if out.exists():
        parser.error("preserve evidence: output must be new")
    original = json.loads((sdk / "summary.json").read_text())
    package = sdk / "DuckDBRust"
    target = None
    minimum = original["minimum_macos"]
    consumer_platform = "macOS"
    expected_platform = 1
    if args.triple:
        target = re.fullmatch(r"(arm64|x86_64)-apple-(macosx|ios)(\d+(?:\.\d+){0,2})(-simulator)?", args.triple)
        if not target:
            parser.error("--triple must select macOS or iOS arm64/x86_64")
        minimum = target[3]
        if target[2] == "ios":
            consumer_platform = "iOS"
            expected_platform = 7 if target[4] else 2
            if not args.apple_sdk or not args.build_only:
                parser.error("iOS requires --apple-sdk and --build-only; runtime verification is separate")
            if target[1] == "x86_64" and not target[4]:
                parser.error("Intel iOS requires the simulator target")
        elif target[4]:
            parser.error("macOS has no simulator target")
    if original.get("slices") and not args.certificate_sha256:
        parser.error("the five-slice SDK requires --certificate-sha256")
    if args.certificate_sha256:
        verify_sdk(sdk, args.certificate_sha256, out.parent)
    for name, expected in original["files"].items():
        assert digest(package / name) == expected, name
    out.mkdir(parents=True)
    relocated = out / "relocated/DuckDBRust"
    shutil.copytree(package, relocated, symlinks=True)
    client = out / "client"
    (client / "Sources/Consumer").mkdir(parents=True)
    shutil.copy2(workspace / "scripts/swift-tests/Consumer.swift", client / "Sources/Consumer/main.swift")
    (client / "Package.swift").write_text('''// swift-tools-version: 5.7
import PackageDescription
let package = Package(
    name: "Consumer",
    platforms: [.PLATFORM("MINIMUM")],
    dependencies: [.package(path: "../relocated/DuckDBRust")],
    targets: [.executableTarget(name: "Consumer", dependencies: [
        .product(name: "DuckDB", package: "DuckDBRust")])]
)
'''.replace("MINIMUM", minimum).replace("PLATFORM", consumer_platform))
    dirs = {name: out / name for name in
            ["build", "cache", "config", "security", "tmp", "module-cache", "fixtures", "foundation-home"]}
    for directory in dirs.values():
        directory.mkdir()
    env = dict(os.environ, TMPDIR=str(dirs["tmp"]),
               SWIFTPM_MODULECACHE_OVERRIDE=str(dirs["module-cache"]),
               CLANG_MODULE_CACHE_PATH=str(dirs["module-cache"]))
    command = ["nice", "-n", "10", "swift", "build", "--jobs", "1", "--package-path", str(client),
               "--scratch-path", str(dirs["build"]), "--cache-path", str(dirs["cache"]),
               "--config-path", str(dirs["config"]), "--security-path", str(dirs["security"])]
    if args.triple:
        command += ["--triple", args.triple]
    if args.apple_sdk:
        command += ["--sdk", str(args.apple_sdk.resolve())]
    report = {"command": command, "original_package": str(package), "relocated": str(relocated),
              "consumer_source_sha256": digest(client / "Sources/Consumer/main.swift"),
              "target_triple": args.triple,
              "minimum_macos_consumer": minimum if consumer_platform == "macOS" else None,
              "consumer_platform": consumer_platform, "minimum_deployment": minimum,
              "build_only": args.build_only,
              "sdk_certificate_sha256": args.certificate_sha256}
    (out / "manifest.json").write_text(json.dumps(report, indent=2) + "\n")
    with (out / "build.log").open("x") as log:
        built = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
    (out / "build-terminal.json").write_text(json.dumps({"exit_code": built.returncode}) + "\n")
    if built.returncode:
        raise SystemExit(built.returncode)
    location = subprocess.check_output(command + ["--show-bin-path"], env=env, text=True).strip()
    executable = Path(location) / "Consumer"
    architecture = subprocess.check_output(["lipo", "-archs", str(executable)], text=True).strip()
    if target:
        assert architecture == target[1], "consumer target architecture mismatch"
    with executable.open("rb") as stream:
        header = stream.read(32)
        magic, _, _, kind, count, size, _, _ = struct.unpack("<8I", header)
        assert magic == 0xFEEDFACF and kind == 2 and size <= 1024 * 1024
        commands = stream.read(size)
    build_versions = []
    offset = 0
    for _ in range(count):
        assert offset + 8 <= len(commands)
        code, length = struct.unpack_from("<2I", commands, offset)
        assert length >= 8 and offset + length <= len(commands)
        if code == 0x32:
            assert length >= 24
            build_versions.append(struct.unpack_from("<3I", commands, offset + 8))
        offset += length
    assert offset == len(commands) and len(build_versions) == 1
    assert build_versions[0][0] == expected_platform, "consumer Mach-O platform differs from target"
    dependencies = subprocess.check_output(["otool", "-L", str(executable)], text=True)
    assert "@rpath/Cduckdb.framework/Versions/A/Cduckdb" in dependencies or "@rpath/Cduckdb.framework/Cduckdb" in dependencies
    assert "libduckdb.dylib" not in dependencies
    subprocess.run([str(workspace / "scripts/sign.sh"), str(executable)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(executable)], check=True)
    executable_hash = digest(executable)
    if args.certificate_sha256:
        shutil.copy2(sdk / "rustdb-dev.pem", out / "rustdb-dev.pem")
        payloads = [executable, out / "manifest.json", client / "Package.swift",
                    client / "Sources/Consumer/main.swift"]
        manifest = out / "SHA256SUMS"
        manifest.write_text("".join(digest(p) + "  " + p.relative_to(out).as_posix() + "\n"
                                    for p in sorted(payloads)))
        subprocess.run(["codesign", "--force", "--sign", "rustdb-dev", "--timestamp=none",
                        "--identifier", "dev.rustdb.SwiftPackageConsumerManifest", "--detached",
                        str(out / "SHA256SUMS.csig"), str(manifest)], check=True)
        verify_manifest(out, args.certificate_sha256, dirs["tmp"])
    if args.build_only:
        for name, expected in original["files"].items():
            assert digest(package / name) == expected, name
            assert digest(relocated / name) == expected, name
        if args.certificate_sha256:
            verify_sdk(sdk, args.certificate_sha256, dirs["tmp"])
            verify_manifest(out, args.certificate_sha256, dirs["tmp"])
        report.update({"build_exit_code": 0, "run_exit_code": None,
                       "executable": str(executable), "architecture": architecture,
                       "build_version": {"platform": build_versions[0][0],
                                         "minimum_encoded": build_versions[0][1],
                                         "sdk_encoded": build_versions[0][2]},
                       "load_commands": dependencies, "executable_sha256": executable_hash,
                       "original_and_relocated_package_hashes_unchanged": True,
                       "runtime_verified": False,
                       "scope": consumer_platform + " consumer compile/sign proof only; runtime/device provisioning unverified"})
        (out / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({k: v for k, v in report.items() if k not in ["command", "load_commands"]}))
        return
    env["CFFIXED_USER_HOME"] = str(dirs["foundation-home"])
    with (out / "run.log").open("x") as log:
        run = subprocess.run(["nice", "-n", "10", str(executable), str(dirs["fixtures"])],
                             env=env, stdout=log, stderr=subprocess.STDOUT, timeout=90)
    (out / "run-terminal.json").write_text(json.dumps({"exit_code": run.returncode}) + "\n")
    if run.returncode:
        raise SystemExit(run.returncode)
    actual = json.loads((dirs["fixtures"] / "consumer.json").read_text())
    loaded = Path(actual["function_image"])
    assert loaded.is_relative_to(out), loaded
    subprocess.run(["codesign", "--verify", "--strict", str(loaded)], check=True)
    framework_hashes = {value for name, value in original["files"].items()
                        if name.endswith("Versions/A/Cduckdb")}
    assert digest(loaded) in framework_hashes
    for name, expected in original["files"].items():
        assert digest(package / name) == expected, name
        assert digest(relocated / name) == expected, name
    assert digest(executable) == executable_hash
    report.update({"build_exit_code": built.returncode, "run_exit_code": run.returncode,
                   "executable": str(executable), "architecture": architecture,
                   "load_commands": dependencies, "executable_sha256": executable_hash,
                   "loaded_framework_sha256": digest(loaded), "consumer": actual,
                   "original_and_relocated_package_hashes_unchanged": True,
                   "scope": "macOS " + architecture + " relocated consumer; other platforms and WAL gates pending"})
    if args.certificate_sha256:
        verify_sdk(sdk, args.certificate_sha256, dirs["tmp"])
        verify_manifest(out, args.certificate_sha256, dirs["tmp"])
    (out / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: value for key, value in report.items() if key not in ["command", "load_commands"]}))


if __name__ == "__main__":
    main()
