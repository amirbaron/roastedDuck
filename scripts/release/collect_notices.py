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
import time
import tomllib
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument("--out", type=Path, required=True)
args = ap.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
inventory = []


def fetch(url, headers=None):
    """GET with retries: shared CI runners hit transient rate limits and resets."""
    for attempt in range(6):
        try:
            return urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=90).read()
        except (urllib.error.URLError, TimeoutError) as error:
            if attempt == 5:
                raise
            print(f"retrying {url} after {error}", flush=True)
            time.sleep(10 * 2 ** attempt)


for package in tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]:
    if not package.get("source", "").startswith("registry+"):
        continue
    name, version = package["name"], package["version"]
    key = f"{name}-{version}"
    url = f"https://static.crates.io/crates/{name}/{key}.crate"
    data = fetch(url)
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
            is_notice = any(marker in relative.name.upper() for marker in ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE"))
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
# The file is fetched from a pinned commit rather than the rate-limited REST API;
# the Git blob hash below identifies its exact bytes either way.
blob_id = "cd496ba72eb37e83a44958358d1f89a8a28cbc15"
data = fetch("https://raw.githubusercontent.com/dropbox/rust-alloc-no-stdlib/"
             "0a81fd6928ea3b33c8cd484aa4575d50ffb98012/LICENSE")
assert hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest() == blob_id
for record in inventory:
    if record["name"] == "alloc-stdlib":
        (args.out / (record["name"] + "-" + record["version"]) / "LICENSE-UPSTREAM").write_bytes(data)
        record["upstream_notice_blob"] = blob_id
        record["notices"].append("LICENSE-UPSTREAM")
(args.out / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n")
