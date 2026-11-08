#!/usr/bin/env python3
"""Collect exact locked crate notices into a caller-selected output directory.

Registry archives are verified against Cargo.lock before their notices are used.
Only notice files and an inventory are retained, not full downloaded sources.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument("--out", type=Path, required=True)
args = ap.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
inventory = []
for package in tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]:
    if not package.get("source", "").startswith("registry+"):
        continue
    name, version = package["name"], package["version"]
    key = f"{name}-{version}"
    url = f"https://static.crates.io/crates/{name}/{key}.crate"
    data = urllib.request.urlopen(url, timeout=90).read()
    assert hashlib.sha256(data).hexdigest() == package["checksum"], key
    dest = args.out / key
    dest.mkdir(exist_ok=True)
    files = []
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        metadata = tomllib.loads(archive.extractfile(key + "/Cargo.toml").read().decode())["package"]
        for member in archive.getmembers():
            relative = Path(member.name).relative_to(key)
            if not member.isfile():
                continue
            is_notice = relative.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE"))
            is_context = str(relative) in ("README.md", "AUTHORS")
            if is_notice or is_context or str(relative) == metadata.get("license-file"):
                assert ".." not in relative.parts
                target = dest / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(archive.extractfile(member).read())
                files.append(str(relative))
    inventory.append({"name": name, "version": version, "checksum": package["checksum"],
                      "license": metadata.get("license"), "notices": files})
    print(key, flush=True)

# alloc-stdlib's crate omits the parent project's BSD notice. Retain the exact
# upstream Git blob shared with alloc-no-stdlib, alongside its own README.
blob_id = "cd496ba72eb37e83a44958358d1f89a8a28cbc15"
request = urllib.request.Request(
    f"https://api.github.com/repos/dropbox/rust-alloc-no-stdlib/git/blobs/{blob_id}",
    headers={"Accept": "application/vnd.github.raw+json"},
)
data = urllib.request.urlopen(request, timeout=30).read()
assert hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest() == blob_id
for record in inventory:
    if record["name"] == "alloc-stdlib":
        (args.out / (record["name"] + "-" + record["version"]) / "LICENSE-UPSTREAM").write_bytes(data)
        record["upstream_notice_blob"] = blob_id
        record["notices"].append("LICENSE-UPSTREAM")
(args.out / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n")
