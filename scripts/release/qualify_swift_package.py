#!/usr/bin/env python3
"""Archive, extract, compile and relocate the actual Linux Swift distribution."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[2]

def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

def verify_archive(path, manifest):
    assert sha(path) == manifest["sha256"]
    files = {}
    with tarfile.open(path) as archive:
        for entry in archive:
            name = str(PurePosixPath(entry.name))
            assert not PurePosixPath(name).is_absolute() and ".." not in PurePosixPath(name).parts
            if entry.isfile():
                assert name not in files
                files[name] = hashlib.file_digest(archive.extractfile(entry), "sha256").hexdigest()
            else:
                assert entry.isdir()
    assert files == manifest["files_sha256"]
    assert files["DuckDBRust/Artifacts/lib/libduckdb.so"] == manifest["native_sha256"]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--expected-sha256", required=True)
    parser.add_argument("--platform", choices=["linux-arm64", "linux-amd64"], required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.sdk = args.sdk.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    (args.out / "tmp").mkdir()
    artifacts = args.out / "artifacts"
    artifacts.mkdir()
    library = args.sdk / "lib/libduckdb.so"
    assert sha(library) == args.expected_sha256
    producer = json.loads((args.sdk.parent / "build-provenance.json").read_text())
    assert producer["platform"] == args.platform and producer["files_sha256"]["sdk/lib/libduckdb.so"] == args.expected_sha256
    with (args.out / "package.log").open("w") as log:
        subprocess.run([sys.executable, str(ROOT / "scripts/swift-tests/package-linux.py"), "--source", str(args.source),
                        "--signed-engine", str(args.sdk), "--expected-native-sha256", args.expected_sha256,
                        "--out", str(args.out / "sdk"), "--tmpdir", str(args.out / "tmp")], stdout=log, stderr=subprocess.STDOUT, check=True)
    version = json.loads((ROOT / "release.json").read_text())["version"]
    archive = artifacts / ("roastedduck-" + version + "-swift-" + args.platform + ".tar.gz")
    with tarfile.open(archive, "w:gz") as output:
        output.add(args.out / "sdk", arcname=".")
    manifest = {"version": version, "platform": args.platform, "source_commit": producer["source_commit"],
                "native_sha256": args.expected_sha256, "filename": archive.name, "sha256": sha(archive),
                "files_sha256": {str(p.relative_to(args.out / "sdk")): sha(p) for p in (args.out / "sdk").rglob("*") if p.is_file()}}
    verify_archive(archive, manifest)
    extracted = args.out / "extracted"
    with tarfile.open(archive) as source:
        source.extractall(extracted, filter="data")
    consumer = args.out / "consumer"
    with (args.out / "consumer-build.log").open("w") as log:
        subprocess.run([sys.executable, str(ROOT / "scripts/swift-tests/consumer-linux-build.py"), "--sdk", str(extracted),
                        "--expected-library-sha256", args.expected_sha256, "--out", str(consumer)], stdout=log, stderr=subprocess.STDOUT, check=True)
    built = json.loads((consumer / "summary.json").read_text())
    assert built["build_exit_code"] == 0 and built["execution_requires_detached_signature"] is False
    assert built["library_sha256"] == args.expected_sha256
    deployed = args.out / "deployed"
    (deployed / "lib").mkdir(parents=True)
    shutil.copy2(built["binary"], deployed / "Consumer")
    shutil.copy2(built["library"], deployed / "lib/libduckdb.so")
    fixtures = args.out / "fixtures"
    fixtures.mkdir()
    env = dict(os.environ, TZ="UTC", TMPDIR=str(args.out / "tmp"))
    env.pop("LD_LIBRARY_PATH", None)
    env.pop("LD_PRELOAD", None)
    with (args.out / "consumer-run.log").open("w") as log:
        subprocess.run([str(deployed / "Consumer"), str(fixtures)], env=env, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=90)
    identity = json.loads((fixtures / "consumer.json").read_text())
    expected = str(deployed / "lib/libduckdb.so")
    assert identity["function_image"] == expected and identity["engine_images"] == [expected]
    assert identity["ids"] == [1, 2, 3] and identity["labels"] == ["prépared שלום", "appended", None]
    assert {"decimal", "blob", "timestamp", "list-null", "struct-unicode", "file-reopen", "result-ownership"} <= set(identity["checks"])
    assert sha(deployed / "lib/libduckdb.so") == sha(library) == args.expected_sha256
    verify_archive(archive, manifest)
    manifest.update(passed=True, extracted_consumer_verified=True, standalone_consumer_verified=True,
                    consumer=identity, consumer_binary_sha256=sha(deployed / "Consumer"))
    (artifacts / "swift-distribution.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (args.out / "verification.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print("Verified extracted and relocated Linux Swift distribution", args.platform)

if __name__ == "__main__":
    main()
