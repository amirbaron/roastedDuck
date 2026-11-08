#!/usr/bin/env python3
"""Validate wheel metadata/RECORD and original-vs-public native entry digests."""
import argparse
import base64
import csv
import hashlib
import io
import json
from pathlib import Path
import zipfile

ap = argparse.ArgumentParser()
ap.add_argument("directory", type=Path)
args = ap.parse_args()
provenance = json.loads((args.directory / "provenance.json").read_text())
version = provenance["release"]
for record in provenance["wheels"]:
    with zipfile.ZipFile(args.directory / record["public"]) as wheel:
        info = f"roastedduck-{version}.dist-info"
        metadata = wheel.read(info + "/METADATA").decode()
        assert "Name: roastedduck\n" in metadata and f"Version: {version}\n" in metadata
        rows = list(csv.reader(io.StringIO(wheel.read(info + "/RECORD").decode())))
        assert {r[0] for r in rows} == set(wheel.namelist())
        for name, checksum, size in rows:
            if not checksum:
                assert name == info + "/RECORD"
                continue
            data = wheel.read(name)
            actual = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
            assert checksum == "sha256=" + actual and int(size) == len(data), name
        for native in record["native_entries_unchanged"]:
            assert hashlib.sha256(wheel.read(native["path"])).hexdigest() == native["sha256"]
    print(record["public"], "verified")
assert len(provenance["wheels"]) == 15
