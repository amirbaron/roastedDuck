#!/usr/bin/env python3
"""Validate actual release metadata, wheel contents/RECORD, and source archives."""
import argparse
import base64
import csv
import email
import email.policy
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import tarfile
import zipfile

from packaging.specifiers import SpecifierSet

ROOT = Path(__file__).resolve().parents[2]
RELEASE = json.loads((ROOT / "release.json").read_text())
VERSION = RELEASE["version"]


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def metadata(data):
    message = email.message_from_bytes(data, policy=email.policy.default)
    assert message["Name"] == "roastedduck"
    assert message["Version"] == VERSION
    assert (message["License-Expression"] or message["License"]) == "MIT"
    spec = SpecifierSet(message["Requires-Python"])
    assert all(f"3.{minor}" in spec for minor in range(10, 15))
    assert "3.9" not in spec and "3.15" not in spec
    assert message["Description-Content-Type"].split(";", 1)[0].strip() == "text/markdown"
    body = message.get_payload()
    assert isinstance(body, str) and len(body) > 1000
    for text in ["pip install roastedduck", "separate environments", "public preview"]:
        assert text in body, f"Missing PyPI reader information: {text}"
    urls = dict(item.split(", ", 1) for item in message.get_all("Project-URL", []))
    assert urls["Repository"] == "https://github.com/amirbaron/roastedDuck"
    assert urls["Documentation"] == "https://amirbaron.github.io/roastedDuck/"
    assert urls["Issues"] == "https://github.com/amirbaron/roastedDuck/issues"
    return {"name": message["Name"], "version": message["Version"],
            "requires_python": str(spec), "description_bytes": len(body.encode()), "project_urls": urls}


def wheel(path):
    match = re.fullmatch(r"roastedduck-" + re.escape(VERSION) + r"-(cp31[0-4])-\1-(.+)\.whl", path.name)
    assert match, f"Unexpected wheel filename: {path.name}"
    abi, platform = match.groups()
    assert platform in {"macosx_11_0_arm64", "manylinux_2_28_aarch64", "manylinux_2_28_x86_64"}, platform
    dist = f"roastedduck-{VERSION}.dist-info"
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        assert len(names) == len(set(names)), "Duplicate wheel entries"
        for name in names:
            assert not name.startswith("/") and ".." not in Path(name).parts, name
        report = metadata(archive.read(dist + "/METADATA"))
        rows = list(csv.reader(io.StringIO(archive.read(dist + "/RECORD").decode())))
        assert {row[0] for row in rows} == set(names)
        for name, expected, size in rows:
            if not expected:
                assert name == dist + "/RECORD"
                continue
            data = archive.read(name)
            actual = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
            assert expected == "sha256=" + actual and int(size) == len(data), name
        assert "_duckdb.py" in names
        assert any(n.startswith("_duckdb-stubs/") for n in names)
        assert "adbc_driver_duckdb/__init__.py" in names
        assert any(n.startswith(dist + "/licenses/") and n.endswith("LICENSE") for n in names)
        assert dist + "/licenses/project/crates/common/data/utf8proc-LICENSE" in names
        native = [n for n in names if n.startswith("duckdb/_duckdb") and n.endswith(".so")]
        assert len(native) == 1, native
        module = archive.read(native[0])
        if platform.startswith("macos"):
            assert module[:4] == b"\xcf\xfa\xed\xfe"
            assert struct.unpack_from("<I", module, 4)[0] == 0x0100000C
        else:
            assert module[:4] == b"\x7fELF" and module[4] == 2 and module[5] == 1
            assert struct.unpack_from("<H", module, 18)[0] == (183 if platform.endswith("aarch64") else 62)
        report.update(abi=abi, platform=platform, native_entry=native[0],
                      native_sha256=hashlib.sha256(module).hexdigest())
    return report


def sdist(path):
    assert path.name == f"roastedduck-{VERSION}.tar.gz"
    with tarfile.open(path) as archive:
        names = archive.getnames()
        for name in names:
            parts = Path(name).parts
            assert not name.startswith("/") and ".." not in parts, name
            assert not {".context", ".git", "target", "__pycache__"}.intersection(parts), name
        roots = {Path(name).parts[0] for name in names}
        assert roots == {f"roastedduck-{VERSION}"}
        pkg_info = f"roastedduck-{VERSION}/PKG-INFO"
        report = metadata(archive.extractfile(pkg_info).read())
        for required in ["Cargo.lock", "pyproject.toml", "PYPI_README.md"]:
            assert any(Path(n).name == required for n in names), required
        assert any("miniz_oxide" in n and n.endswith("Cargo.toml") for n in names)
        assert any("ureq-proto" in n and n.endswith("Cargo.toml") for n in names)
        report["files"] = len(names)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--complete", action="store_true")
    args = parser.parse_args()
    records = []
    for path in sorted(args.directory.iterdir()):
        if path.suffix == ".whl":
            record = wheel(path)
        elif path.name.endswith(".tar.gz"):
            record = sdist(path)
        else:
            continue
        assert path.stat().st_size <= 100_000_000, "Exceeds default PyPI file-size limit"
        record.update(filename=path.name, sha256=sha(path), bytes=path.stat().st_size)
        records.append(record)
    assert records, "No distributions found"
    if args.complete:
        assert len([r for r in records if "abi" in r]) == 15
        assert len([r for r in records if "abi" not in r]) == 1
        assert len({(r["abi"], r["platform"]) for r in records if "abi" in r}) == 15
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps({"version": VERSION, "distributions": records}, indent=2) + "\n")
    print(f"Validated {len(records)} distributions for {VERSION}")


if __name__ == "__main__":
    main()
