"""Build/sign pinned Go client tests with Arrow and an explicit dynamic engine."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import sys


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def verify_native(path, signed_root=None, certificate_sha256=None, tmpdir=None):
    """Linux trust must come from the caller, never from package provenance."""
    if sys.platform == "darwin":
        subprocess.run(["codesign", "--verify", "--strict", str(path)], check=True)
        return
    if not sys.platform.startswith("linux"):
        raise ValueError("unsupported client-test host")
    if not (signed_root and certificate_sha256 and tmpdir):
        raise ValueError("Linux requires a signed root, trusted certificate pin and external tmpdir")
    root = Path(signed_root).resolve()
    path = path.resolve()
    if not path.is_relative_to(root):
        raise ValueError("selected artifact is outside the signed directory")
    signing = Path(__file__).resolve().parents[1] / "signing"
    sys.path.insert(0, str(signing))
    from verify_detached_manifest import verify

    verify(root / "SHA256SUMS", root / "SHA256SUMS.csig", root / "rustdb-dev.pem",
           "openssl", Path(tmpdir), certificate_sha256)
    relative = path.relative_to(root).as_posix()
    entries = dict((line[66:], line[:64]) for line in (root / "SHA256SUMS").read_text().splitlines())
    if entries.get(relative) != digest(path):
        raise ValueError("selected artifact is not authenticated by the manifest")


def linux_dependencies(executable, library, env):
    loader_env = dict(env)
    loader_env.pop("LD_LIBRARY_PATH", None)
    result = subprocess.run(["ldd", str(executable)], env=loader_env, text=True,
                            capture_output=True, check=True)
    engines = []
    for line in result.stdout.splitlines():
        match = re.match(r"\s*(.*?)\s+\(0x[0-9a-fA-F]+\)\s*$", line)
        if not match:
            continue
        dependency = match[1].split(" => ", 1)[-1]
        if Path(dependency).name == "libduckdb.so":
            engines.append(Path(dependency).resolve())
    if engines != [library.resolve()] or "not found" in result.stdout:
        raise ValueError("selected engine or dynamic dependency mismatch: " + result.stdout)
    return result.stdout


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--go", type=Path, required=True)
    parser.add_argument("--library", type=Path, required=True)
    parser.add_argument("--shared-cache", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--goarch", choices=["arm64", "amd64"],
                        help="defaults to the native host architecture; selected library must match")
    parser.add_argument("--signed-library-root", type=Path,
                        help="Linux manifest directory containing the selected library")
    parser.add_argument("--certificate-sha256", help="trusted Linux signing certificate fingerprint")
    parser.add_argument("--diagnostic-source", action="store_true",
                        help="permit recorded source changes for explicitly labeled diagnostics")
    args = parser.parse_args()
    source, go, library, cache, out = [path.resolve() for path in
        [args.source, args.go, args.library, args.shared_cache, args.out]]
    if out.exists():
        parser.error("preserve evidence: output must be new")
    if sys.platform not in ["darwin", "linux"]:
        parser.error("this harness supports macOS and Linux")
    goos = "darwin" if sys.platform == "darwin" else "linux"
    machine = platform.machine().lower()
    goarch = args.goarch or {"arm64": "arm64", "aarch64": "arm64",
                             "x86_64": "amd64", "amd64": "amd64"}.get(machine)
    if not goarch:
        parser.error("unsupported host architecture; supply --goarch explicitly")
    if goos == "linux":
        if not args.signed_library_root or not args.certificate_sha256:
            parser.error("Linux requires --signed-library-root and --certificate-sha256")
        with library.open("rb") as stream:
            header = stream.read(64)
        assert header[:6] == b"\x7fELF\x02\x01" and struct.unpack_from("<H", header, 18)[0] == {"arm64": 183, "amd64": 62}[goarch], "selected library architecture mismatch"
    else:
        architecture = subprocess.check_output(["lipo", "-archs", str(library)], text=True).strip()
        assert architecture == {"arm64": "arm64", "amd64": "x86_64"}[goarch], architecture
    verification_tmp = cache / "verification-tmp"
    verification_tmp.mkdir(parents=True, exist_ok=True)
    verify_native(library, args.signed_library_root, args.certificate_sha256, verification_tmp)
    original_hash = digest(library)
    out.mkdir(parents=True)
    package = out / "source"
    shutil.copytree(source, package)
    inputs = {str(p.relative_to(package)): digest(p) for p in sorted(package.rglob("*")) if p.is_file()}
    pin = json.loads((workspace / "go/upstream.json").read_text())
    source_differences = sorted(name for name in inputs.keys() | pin["files"].keys()
                                if inputs.get(name) != pin["files"].get(name))
    assert not source_differences or args.diagnostic_source, source_differences
    directories = {name: cache / name for name in ["gopath", "mod", "build", "telemetry"]}
    directories.update({name: out / name for name in ["tmp", "goenv"]})
    for directory in directories.values():
        directory.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, PATH=str(go.parent) + os.pathsep + os.environ["PATH"],
               GOPATH=str(directories["gopath"]), GOMODCACHE=str(directories["mod"]),
               GOCACHE=str(directories["build"]), GOTMPDIR=str(directories["tmp"]),
               TMPDIR=str(directories["tmp"]), GOTELEMETRYDIR=str(directories["telemetry"]),
               GOENV=str(directories["goenv"] / "env"), GOTOOLCHAIN="local", GOMAXPROCS="2",
               CGO_ENABLED="1", GOOS=goos, GOARCH=goarch, GOFLAGS="",
               # The upstream dynamic bindings also request -lduckdb themselves.
               # Scope that lookup to the same selected image's directory.
               CGO_LDFLAGS='"' + str(library) + '" "-L' + str(library.parent) + '"',
               CGO_LDFLAGS_ALLOW=".*")
    if goos == "linux":
        env["CGO_LDFLAGS"] += ' "-Wl,-rpath,' + str(library.parent) + '"'
        env.pop("LD_LIBRARY_PATH", None)
    executable = out / "duckdb-go.test"
    command = ["nice", "-n", "10", str(go), "test", "-c", "-tags=duckdb_use_lib,duckdb_arrow",
               "-p", "1", "-o", str(executable), "."]
    report = {"command": command, "toolchain": subprocess.check_output([str(go), "version"], text=True).strip(),
              "source": str(source), "source_copy": str(package), "input_hashes": inputs,
              "upstream_pin_sha256": digest(workspace / "go/upstream.json"),
              "pinned_source_verified": not source_differences,
              "diagnostic_source": args.diagnostic_source, "source_differences": source_differences,
              "library": str(library), "library_sha256": original_hash,
              "signed_library_root": str(args.signed_library_root.resolve()) if args.signed_library_root else None,
              "platform": {"goos": goos, "goarch": goarch},
              "environment": {k: env[k] for k in ["GOPATH", "GOMODCACHE", "GOCACHE", "GOTMPDIR", "TMPDIR",
                  "GOTELEMETRYDIR", "GOENV", "GOTOOLCHAIN", "CGO_LDFLAGS", "GOOS", "GOARCH"]}}
    (out / "manifest.json").write_text(json.dumps(report, indent=2) + "\n")
    with (out / "build.log").open("x") as log:
        result = subprocess.run(command, cwd=package, env=env, stdout=log, stderr=subprocess.STDOUT)
    (out / "terminal.json").write_text(json.dumps({"exit_code": result.returncode}) + "\n")
    if result.returncode:
        raise SystemExit(result.returncode)
    header = directories["mod"] / ("github.com/duckdb/duckdb-go-bindings@" + pin["bindings_version"]) / "include/duckdb.h"
    assert digest(header) == pin["bindings_header_sha256"], "bindings header changed"
    if goos == "darwin":
        identifier = subprocess.check_output(["otool", "-D", str(library)], text=True).splitlines()[1].strip()
        if identifier != str(library):
            subprocess.run(["install_name_tool", "-change", identifier, str(library), str(executable)], check=True)
        dependencies = subprocess.check_output(["otool", "-L", str(executable)], text=True)
        engines = [line.strip().split(" (compatibility version", 1)[0] for line in dependencies.splitlines()[1:]
                   if "libduckdb.dylib" in line]
        assert engines == [str(library)], engines
        subprocess.run([str(workspace / "scripts/sign.sh"), str(executable)], check=True)
        verify_native(executable)
    else:
        dependencies = linux_dependencies(executable, library, env)
    verify_native(library, args.signed_library_root, args.certificate_sha256, verification_tmp)
    assert digest(library) == original_hash
    for name, expected in inputs.items():
        assert digest(package / name) == expected
    report.update({"exit_code": result.returncode, "executable": str(executable),
                   "executable_sha256": digest(executable), "load_commands": dependencies,
                   "tests_executed": False, "scope": "Pinned Go root-suite build with Arrow; tests not yet run"})
    report["test_executable_requires_detached_signature"] = goos == "linux"
    (out / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k: report[k] for k in ["executable", "executable_sha256", "tests_executed", "exit_code"]}))


if __name__ == "__main__":
    main()
