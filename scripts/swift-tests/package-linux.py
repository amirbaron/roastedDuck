"""Prepare a relocatable Linux Swift package around an authenticated engine."""

import argparse
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys

from adapt import apply_source_adaptations
from prepare import digest


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--signed-engine", required=True, type=Path)
    parser.add_argument("--certificate-sha256", required=True)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--tmpdir", required=True, type=Path)
    args = parser.parse_args()
    source, engine, out = args.source.resolve(), args.signed_engine.resolve(), args.out.resolve()
    if out.exists():
        parser.error("preserve previous evidence: --out must be new")
    verifier = workspace / "scripts/signing/verify_detached_manifest.py"
    verify = [sys.executable, str(verifier), str(engine / "SHA256SUMS"),
              str(engine / "SHA256SUMS.csig"), str(engine / "rustdb-dev.pem"),
              "--certificate-sha256", args.certificate_sha256, "--tmpdir", str(args.tmpdir)]
    subprocess.run(verify, check=True)
    library = engine / "lib/libduckdb.so"
    with library.open("rb") as stream:
        elf = stream.read(20)
    assert elf[:6] == b"\x7fELF\x02\x01", "ELF64 little-endian required"
    machine = struct.unpack_from("<H", elf, 18)[0]
    assert machine in [62, 183], "only Linux x86_64/arm64 in scope"
    pin = json.loads((workspace / "swift/upstream.json").read_text())
    client = source / "tools/swift/duckdb-swift"
    header = source / "src/include/duckdb.h"
    assert digest(header) == pin["header_sha256"]
    for item in pin["files"]:
        assert digest(client / item["path"]) == item["sha256"], item["path"]
    out.mkdir(parents=True)
    package = out / "DuckDBRust"
    for item in pin["files"]:
        if item["path"].startswith("Tests/"):
            continue
        destination = package / item["path"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(client / item["path"], destination)
    adaptations = apply_source_adaptations(package)
    include = package / "Sources/Cduckdb/include"
    include.mkdir(parents=True)
    shutil.copy2(header, include / "duckdb.h")
    (include.parent / "Cduckdb.c").write_text('#include "duckdb.h"\n')
    native = package / "Artifacts/lib/libduckdb.so"
    native.parent.mkdir(parents=True)
    shutil.copy2(library, native)
    # Resolve the package path at manifest evaluation, after relocation. The
    # basename link avoids embedding the original selected engine's full path.
    (package / "Package.swift").write_text('''// swift-tools-version: 5.7
import Foundation
import PackageDescription
let native = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    .appendingPathComponent("Artifacts/lib").path
let package = Package(
    name: "DuckDBRust",
    products: [.library(name: "DuckDB", targets: ["DuckDB"])],
    targets: [
        .target(name: "Cduckdb", publicHeadersPath: "include"),
        .target(name: "DuckDB", dependencies: ["Cduckdb"], linkerSettings: [
            .unsafeFlags(["-L", native, "-Xlinker", "-rpath", "-Xlinker", "$ORIGIN/lib",
                          "-Xlinker", "-rpath", "-Xlinker", native]),
            .linkedLibrary("duckdb")])
    ]
)
''')
    assert digest(native) == digest(library)
    subprocess.run(verify, check=True)
    files = {str(p.relative_to(package)): digest(p) for p in sorted(package.rglob("*")) if p.is_file()}
    report = {"package": str(package), "original_library": str(library),
              "library_sha256": digest(library), "architecture": {62: "x86_64", 183: "arm64"}[machine],
              "source_adaptations": adaptations, "files": files,
              "scope": "Linux Swift source/native package preparation; consumer runtime and deployment verification required"}
    (out / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k: v for k, v in report.items() if k != "files"}))


if __name__ == "__main__":
    main()
