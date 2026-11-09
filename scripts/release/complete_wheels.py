#!/usr/bin/env python3
"""Add complete third-party notices to freshly built wheels, preserving native bytes."""
import argparse
import base64
import csv
import hashlib
import io
import json
from pathlib import Path
import zipfile

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--input", type=Path, required=True)
parser.add_argument("--notices", type=Path, required=True)
parser.add_argument("--out", type=Path, required=True)
parser.add_argument("--report", type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
notices = {"PROJECT-LICENSE": ROOT / "LICENSE", "THIRD_PARTY_NOTICES.md": ROOT / "THIRD_PARTY_NOTICES.md"}
for path in args.notices.rglob("*"):
    if path.is_file():
        notices["rust-dependencies/" + str(path.relative_to(args.notices))] = path
for base in [ROOT / "crates", ROOT / "third_party", ROOT / "python"]:
    for path in base.rglob("*"):
        if path.is_file() and any(marker in path.name.upper() for marker in ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE")):
            notices["project/" + str(path.relative_to(ROOT))] = path
reports = []
for source in sorted(args.input.glob("*.whl")):
    destination = args.out / source.name
    records, native = [], {}
    with zipfile.ZipFile(source) as original, zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as output:
        dist = next(n.rsplit("/", 1)[0] for n in original.namelist() if n.endswith(".dist-info/METADATA"))

        def write(name, data, template=None):
            info = template or zipfile.ZipInfo(name, date_time=(2026, 11, 9, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            if template is None:
                info.external_attr = 0o644 << 16
            output.writestr(info, data)
            digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
            records.append([name, "sha256=" + digest, str(len(data))])

        for item in original.infolist():
            if item.filename == dist + "/RECORD":
                continue
            data = original.read(item.filename)
            if item.filename == dist + "/METADATA":
                headers, body = data.replace(b"\r\n", b"\n").split(b"\n\n", 1)
                declarations = b"".join(("\nLicense-File: " + name).encode() for name in sorted(notices))
                data = headers + declarations + b"\n\n" + body
            if item.filename.endswith((".so", ".dylib")):
                native[item.filename] = hashlib.sha256(data).hexdigest()
            write(item.filename, data, item)
        for name, path in sorted(notices.items()):
            target = dist + "/licenses/" + name
            assert target not in original.namelist(), target
            write(target, path.read_bytes())
        stream = io.StringIO(newline="")
        csv.writer(stream, lineterminator="\n").writerows(records + [[dist + "/RECORD", "", ""]])
        output.writestr(dist + "/RECORD", stream.getvalue())
    with zipfile.ZipFile(destination) as final:
        for name, expected in native.items():
            assert hashlib.sha256(final.read(name)).hexdigest() == expected
    reports.append({"wheel": destination.name, "original_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                    "final_sha256": hashlib.sha256(destination.read_bytes()).hexdigest(),
                    "native_bytes_unchanged": native, "added_notice_files": len(notices)})
assert reports, "No freshly built wheels found"
args.report.parent.mkdir(parents=True, exist_ok=True)
args.report.write_text(json.dumps(reports, indent=2) + "\n")
print("Completed notices for", len(reports), "wheels; native bytes preserved")
