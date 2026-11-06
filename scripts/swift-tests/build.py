"""Build and sign a prepared Swift test package, without executing its tests."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_inputs(package, preparation, pin):
    library = Path(preparation["library"])
    assert digest(library) == preparation["library_sha256"], "selected library changed"
    assert digest(package / "Package.swift") == preparation["manifest_sha256"], "manifest changed"
    assert digest(package / "Sources/Cduckdb/include/duckdb.h") == pin["header_sha256"]
    adaptations = {item["path"]: item for item in preparation.get("fixture_adaptations", [])}
    adaptations.update({item["path"]: item for item in preparation.get("source_adaptations", [])})
    for item in pin["files"]:
        adaptation = adaptations.get(item["path"])
        if adaptation:
            assert adaptation["original_sha256"] == item["sha256"]
        expected = adaptation["patched_sha256"] if adaptation else item["sha256"]
        assert digest(package / item["path"]) == expected, item["path"]
    for item in preparation.get("additional_test_files", []):
        assert digest(package / item["path"]) == item["sha256"], item["path"]
    for item in preparation.get("test_infrastructure_files", []):
        assert digest(package / item["path"]) == item["sha256"], item["path"]
    return library


def load_commands(binary):
    text = subprocess.check_output(["otool", "-L", str(binary)], text=True)
    dependencies = [line.strip().split(" (compatibility version", 1)[0]
                    for line in text.splitlines()[1:]]
    return text, dependencies


def project_macho(path):
    with path.open("rb") as handle:
        header = handle.read(16)
    if len(header) < 16:
        return False
    if header[:4] == b"\xcf\xfa\xed\xfe":
        return struct.unpack("<I", header[12:16])[0] in [2, 6, 8]
    if header[:4] == b"\xfe\xed\xfa\xcf":
        return struct.unpack(">I", header[12:16])[0] in [2, 6, 8]
    return False


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--triple", help="explicit macOS Swift target, for example x86_64-apple-macosx11.0")
    args = parser.parse_args()
    package, output = args.package.resolve(), args.out.resolve()
    if output.exists():
        parser.error("use a fresh --out path to preserve previous evidence")
    processes = subprocess.check_output(["ps", "-axo", "pid,command"]).decode("utf-8", "replace")
    live_corpus = [line for line in processes.splitlines() if re.search(r"/slt run\s", line)]
    if live_corpus:
        parser.error("SQL corpus is still live; finish that workload first: " + live_corpus[0].strip())
    preparation = json.loads((package / "preparation.json").read_text())
    pin_path = workspace / "swift/upstream.json"
    assert digest(pin_path) == preparation["source_pin_sha256"]
    pin = json.loads(pin_path.read_text())
    library = verify_inputs(package, preparation, pin)
    target_architecture = None
    if args.triple:
        match = re.fullmatch(r"(arm64|x86_64)-apple-macosx\d+(?:\.\d+){0,2}", args.triple)
        if sys.platform != "darwin" or not match:
            parser.error("--triple currently requires an explicit macOS arm64/x86_64 target")
        target_architecture = match[1]
        architectures = subprocess.check_output(["lipo", "-archs", str(library)], text=True).split()
        assert architectures == [target_architecture], "selected library architecture differs from target"
        commands = subprocess.check_output(["otool", "-l", str(library)], text=True)
        build_version = commands.split("cmd LC_BUILD_VERSION\n", 1)[1].split("Load command", 1)[0]
        assert re.search(r"\bplatform 1\s", build_version), "macOS library required"
    output.mkdir(parents=True)
    directories = {name: output / name for name in
                   ["build", "cache", "config", "security", "tmp", "module-cache"]}
    for directory in directories.values():
        directory.mkdir()
    command = ["nice", "-n", "10", "swift", "build", "--package-path", str(package),
               "--scratch-path", str(directories["build"]), "--cache-path", str(directories["cache"]),
               "--config-path", str(directories["config"]), "--security-path", str(directories["security"]),
               "--jobs", "1", "--build-tests"]
    if args.triple:
        command += ["--triple", args.triple]
    env = dict(os.environ, TMPDIR=str(directories["tmp"]),
               SWIFTPM_MODULECACHE_OVERRIDE=str(directories["module-cache"]),
               CLANG_MODULE_CACHE_PATH=str(directories["module-cache"]))
    report = {"started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "package": str(package), "preparation": preparation, "command": command,
              "target_triple": args.triple, "target_architecture": target_architecture,
              "build_exit_code": None, "tests_executed": False,
              "scope": "Build/sign readiness only; test and portable-package acceptance remain pending"}
    (output / "manifest.json").write_text(json.dumps(report, indent=2) + "\n")
    with (output / "build.log").open("x") as log:
        result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
    report["build_exit_code"] = result.returncode
    (output / "build-terminal.json").write_text(json.dumps({"exit_code": result.returncode}, indent=2) + "\n")
    if result.returncode:
        (output / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report))
        raise SystemExit(result.returncode)
    native = []
    if sys.platform == "darwin":
        subprocess.run(["codesign", "--verify", "--strict", str(library)], check=True)
        install_id = subprocess.check_output(["otool", "-D", str(library)], text=True).splitlines()[1].strip()
        for binary in sorted(directories["build"].rglob("*")):
            if not binary.is_file() or binary.is_symlink() or not project_macho(binary):
                continue
            before, dependencies = load_commands(binary)
            if install_id in dependencies and install_id != str(library):
                subprocess.run(["install_name_tool", "-change", install_id, str(library), str(binary)], check=True)
            after, dependencies = load_commands(binary)
            duckdb_images = [item for item in dependencies if Path(item).name == "libduckdb.dylib"]
            assert not duckdb_images or duckdb_images == [str(library)], (binary, duckdb_images)
            if duckdb_images and target_architecture:
                actual_architectures = subprocess.check_output(["lipo", "-archs", str(binary)], text=True).split()
                assert actual_architectures == [target_architecture], (binary, actual_architectures)
            subprocess.run([str(workspace / "scripts/sign.sh"), str(binary)], check=True)
            subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
            native.append({"path": str(binary), "sha256": digest(binary),
                           "selected_library_dependency": duckdb_images,
                           "load_commands_before": before, "load_commands_after": after})
        assert any(item["selected_library_dependency"] for item in native), "no project artifact links selected engine"
        for bundle in directories["build"].rglob("*.xctest"):
            if bundle.is_dir() and not bundle.is_symlink():
                subprocess.run([str(workspace / "scripts/sign.sh"), str(bundle)], check=True)
                subprocess.run(["codesign", "--verify", "--strict", str(bundle)], check=True)
        for artifact in native:
            binary = Path(artifact["path"])
            subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
            artifact["sha256"] = digest(binary)
    elif sys.platform.startswith("linux"):
        loader_env = dict(env)
        loader_env.pop("LD_LIBRARY_PATH", None)
        for binary in sorted(directories["build"].rglob("*")):
            if not binary.is_file() or binary.is_symlink():
                continue
            with binary.open("rb") as stream:
                header = stream.read(20)
            if header[:6] != b"\x7fELF\x02\x01" or len(header) < 20:
                continue
            if struct.unpack_from("<H", header, 16)[0] not in [2, 3]:
                continue
            dependencies = subprocess.run(["ldd", str(binary)], env=loader_env,
                                          capture_output=True, text=True)
            images = []
            for line in dependencies.stdout.splitlines():
                match = re.match(r"\s*(.*?)\s+\(0x[0-9a-fA-F]+\)\s*$", line)
                if match:
                    image = match[1].split(" => ", 1)[-1]
                    if Path(image).name == "libduckdb.so":
                        images.append(str(Path(image).resolve()))
            assert not images or images == [str(library)], (binary, dependencies.stdout)
            native.append({"path": str(binary), "sha256": digest(binary),
                           "selected_library_dependency": images,
                           "loader_output": dependencies.stdout + dependencies.stderr})
        assert any(item["selected_library_dependency"] for item in native), "no Linux artifact links selected engine"
        report["detached_signature_required_before_execution"] = True
    verify_inputs(package, preparation, pin)
    report.update({"ended_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                   "native_artifacts": native, "source_and_library_unchanged": True})
    (output / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: value for key, value in report.items() if key != "native_artifacts"}))


if __name__ == "__main__":
    main()
