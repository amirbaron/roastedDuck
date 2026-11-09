#!/usr/bin/env python3
"""Pin new Rust SDK and official C++/HTTPFS controls without importing either here."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--platform", choices=["macos-arm64", "linux-arm64", "linux-amd64"], required=True)
parser.add_argument("--rust-library", type=Path, required=True)
parser.add_argument("--rust-sha256", required=True)
parser.add_argument("--cpp-python", type=Path, required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out = args.out.resolve()
args.out.mkdir(parents=True, exist_ok=False)
pin = json.loads((ROOT / "scripts/release/reference-pins.json").read_text())[args.platform]


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


assert sha(args.rust_library) == args.rust_sha256
url = "https://github.com/duckdb/duckdb/releases/download/v1.5.6/" + pin["archive"]
archive = args.out / pin["archive"]
with urllib.request.urlopen(url, timeout=90) as response, archive.open("wb") as output:
    shutil.copyfileobj(response, output)
assert sha(archive) == pin["archive_sha256"]
paths = {}
for role in ["rust", "cpp"]:
    directory = args.out / role / "lib"
    directory.mkdir(parents=True)
    paths[role] = directory / pin["library"]
shutil.copy2(args.rust_library, paths["rust"])
with zipfile.ZipFile(archive) as source:
    names = [name for name in source.namelist() if Path(name).name == pin["library"]]
    assert len(names) == 1
    paths["cpp"].write_bytes(source.read(names[0]))
assert sha(paths["cpp"]) == pin["library_sha256"]
transformations = []
if sys.platform == "darwin":
    for role, path in paths.items():
        verification = subprocess.run(["codesign", "--verify", "--strict", str(path)], capture_output=True)
        if verification.returncode:
            original = sha(path)
            subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(path)], check=True)
            subprocess.run(["codesign", "--verify", "--strict", str(path)], check=True)
            transformations.append({"role": role, "operation": "ad-hoc signing only", "before": original, "after": sha(path)})
fetch_report = args.out / "httpfs-fetch.json"
with (args.out / "httpfs-fetch.log").open("w") as log:
    subprocess.run([str(args.cpp_python.absolute()), str(ROOT / "scripts/release/fetch_httpfs_worker.py"),
                    str(args.out / "fetch"), pin["duckdb_platform"],
                    pin["httpfs_sha256"], str(fetch_report)], stdout=log, stderr=subprocess.STDOUT, check=True)
fetched = json.loads(fetch_report.read_text())
directory = args.out / "httpfs"
directory.mkdir()
paths["httpfs"] = directory / "httpfs.duckdb_extension"
shutil.copy2(fetched["httpfs"], paths["httpfs"])
assert sha(paths["httpfs"]) == pin["httpfs_sha256"]
manifest = {
    "platform": args.platform,
    "artifacts": {role: {"path": str(path.relative_to(args.out)), "sha256": sha(path)} for role, path in paths.items()},
    "rust_original_sha256": args.rust_sha256,
    "cpp_reference": {"url": url, "archive_sha256": pin["archive_sha256"], "original_library_sha256": pin["library_sha256"],
                      "scope": "new functional comparator; not the historical performance reference"},
    "httpfs_reference": {"official_client_install": True, "platform": pin["duckdb_platform"],
                         "native_sha256": pin["httpfs_sha256"], "unsafe_crypto_enabled": False, "unsigned_extensions_enabled": False},
    "transformations": transformations,
}
path = args.out / "input-manifest.json"
path.write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps({"manifest": str(path), "sha256": sha(path)}))
