#!/usr/bin/env python3
"""Package the frozen software candidate without rebuilding native engine bytes.

Inputs are a verified candidate payload and an empty output directory. The exact
original manifest is pinned below; new branding has separate artifact hashes.
No credentials or local signing keys are copied into public archives.
"""
from __future__ import annotations

import argparse
import base64
import csv
import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
VERSION = "0.1.0-alpha.1"
PYTHON_VERSION = "0.1.0a1"
MANIFEST_SHA256 = "309821a4b830e46a8f1cb628dd43e83e9e93bf0d041dd716c34db49cec081bb2"
EPOCH = 1793923200  # Stable archive metadata; not a claim about build time.


def digest(path: Path) -> str:
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def archive(destination: Path, files: dict[str, Path | bytes | tuple]) -> None:
    with destination.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=1) as gz:
        with tarfile.open(fileobj=gz, mode="w") as tf:
            for name, source in sorted(files.items()):
                info = tarfile.TarInfo(name)
                info.mtime = EPOCH
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                if isinstance(source, tuple):
                    info.type = tarfile.SYMTYPE
                    info.linkname = source[0]
                    info.mode = 0o777
                    tf.addfile(info)
                elif isinstance(source, bytes):
                    info.size = len(source)
                    info.mode = 0o644
                    tf.addfile(info, io.BytesIO(source))
                else:
                    info.size = source.stat().st_size
                    info.mode = 0o755 if source.stat().st_mode & 0o111 else 0o644
                    with source.open("rb") as stream:
                        tf.addfile(info, stream)


def tree(directory: Path, prefix: str = "") -> dict:
    result = {}
    for path in sorted(directory.rglob("*")):
        name = str(Path(prefix) / path.relative_to(directory))
        if path.is_symlink():
            result[name] = (str(path.readlink()),)
        elif path.is_file():
            result[name] = path
    return result


def rebrand_wheel(source: Path, destination: Path, notices: dict) -> dict:
    new_info = f"roastedduck-{PYTHON_VERSION}.dist-info"
    records = []
    native = []
    with zipfile.ZipFile(source) as original, zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=1) as output:
        def write(name: str, data: bytes, mode: int = 0o644) -> None:
            info = zipfile.ZipInfo(name, date_time=(2026, 11, 6, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = mode << 16
            output.writestr(info, data)
            checksum = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
            records.append([name, "sha256=" + checksum, str(len(data))])

        for item in original.infolist():
            name = item.filename
            if name.endswith(("/RECORD", "/RECORD.jws", "/RECORD.p7s")):
                continue
            data = original.read(name)
            if ".dist-info/" in name:
                name = new_info + "/" + name.split(".dist-info/", 1)[1]
                if name.endswith("/METADATA"):
                    text = data.decode()
                    text = text.replace("Name: duckdb\n", "Name: roastedduck\n")
                    text = text.replace("Version: 1.5.6\n", f"Version: {PYTHON_VERSION}\n")
                    data = text.encode()
            if name == "duckdb/_version.py":
                data = (ROOT / "python/duckdb/_version.py").read_bytes()
            if name.endswith((".so", ".dylib", ".dll", ".pyd")):
                native.append({"path": name, "sha256": hashlib.sha256(data).hexdigest()})
            write(name, data, item.external_attr >> 16)
        for name, path in sorted(notices.items()):
            if isinstance(path, Path):
                write(new_info + "/licenses/" + name, path.read_bytes())
        stream = io.StringIO(newline="")
        csv.writer(stream, lineterminator="\n").writerows(records + [[new_info + "/RECORD", "", ""]])
        output.writestr(new_info + "/RECORD", stream.getvalue())
    assert native, f"No native extension in {source}"
    # Independently reread the repacked native entries before crediting preservation.
    with zipfile.ZipFile(destination) as output:
        for entry in native:
            assert hashlib.sha256(output.read(entry["path"])).hexdigest() == entry["sha256"]
    return {"original": source.name, "original_sha256": digest(source), "public": destination.name,
            "native_entries_unchanged": native, "changed": ["distribution metadata and RECORD", "duckdb/_version.py", "additional license notices"]}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--payload", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--dependency-notices", type=Path, required=True)
    args = ap.parse_args()
    payload, out = args.payload.resolve(), args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if any(out.iterdir()):
        raise SystemExit("Output directory must be empty; never overwrite release artifacts")
    assert digest(payload / "SHA256SUMS") == MANIFEST_SHA256, "Candidate manifest mismatch"
    entries = {}
    for line in (payload / "SHA256SUMS").read_text().splitlines():
        expected, name = line.split(maxsplit=1)
        name = name.lstrip("*")
        path = payload / name
        assert path.resolve().is_relative_to(payload), f"Unsafe candidate path: {name}"
        assert digest(path) == expected, f"Candidate content mismatch: {name}"
        entries[name] = expected
    symlinks = json.loads((payload / "metadata/symlinks.json").read_text())
    for name, target in symlinks.items():
        path = payload / name
        assert path.is_symlink() and str(path.readlink()) == target, f"Candidate symlink mismatch: {name}"
        assert path.resolve().is_relative_to(payload), f"Candidate symlink escapes payload: {name}"
    print(f"Verified {len(entries)} original manifest entries", flush=True)
    notices = {"LICENSE": ROOT / "LICENSE", "THIRD_PARTY_NOTICES.md": ROOT / "THIRD_PARTY_NOTICES.md"}
    notices.update(tree(payload / "notices", "notices"))
    notices.update(tree(args.dependency_notices, "notices/rust-dependencies"))
    provenance = {
        "project": "roastedDuck", "release": VERSION,
        "engine_source_commit": "92a8c862238013be466ded949e50335a63e6a508",
        "candidate": "source-2551", "original_manifest_sha256": MANIFEST_SHA256,
        "packager_sha256": digest(Path(__file__)), "engine_rebuilt": False,
        "physical_ios_execution_verified": False,
        "historical_x86_64_qualification_used_translation": True,
        "original_files_verified": len(entries), "original_symlinks_verified": symlinks, "native": {}, "wheels": [],
        "packaging_checks_are_separate_from_historical_full_suite": True,
    }
    readme = (ROOT / "scripts/release/PACKAGE-README.md").read_bytes()
    for platform in ["macos-arm64", "linux-arm64", "linux-amd64"]:
        base = payload / "native" / platform
        files = {**tree(base), **notices, **tree(payload / "go", "go"), **tree(payload / "adbc", "adbc")}
        files["README.md"] = readme
        files["bin/roastedDuck"] = ("rustdb",)
        static = {k: v for k, v in files.items() if k.endswith(".a")}
        dynamic = {k: v for k, v in files.items() if not k.endswith(".a")}
        archive(out / f"roastedduck-{VERSION}-{platform}.tar.gz", dynamic)
        archive(out / f"roastedduck-{VERSION}-{platform}-static.tar.gz", {**static, **notices, **tree(base / "licenses", "licenses")})
        provenance["native"][platform] = {name: digest(path) for name, path in tree(base).items() if name.startswith(("bin/", "lib/")) and isinstance(path, Path)}
        print("Packaged native", platform, flush=True)
    for platform in ["apple", "linux-arm64", "linux-amd64"]:
        archive(out / f"roastedduck-{VERSION}-swift-{platform}.tar.gz", {**tree(payload / "swift" / platform), **notices, "README.md": readme})
        print("Packaged Swift", platform, flush=True)
    for source in sorted((payload / "python").rglob("*.whl")):
        name = source.name.replace("duckdb-1.5.6-", f"roastedduck-{PYTHON_VERSION}-", 1)
        provenance["wheels"].append(rebrand_wheel(source, out / name, notices))
        print("Packaged wheel", name, flush=True)
    (out / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    archive(out / f"roastedduck-{VERSION}-evidence.tar.gz", {**tree(ROOT / "website/docs/evidence", "evidence"), **notices})
    (out / "SHA256SUMS").write_text("".join(f"{digest(p)}  {p.name}\n" for p in sorted(out.iterdir()) if p.is_file()))
    print("Finished; SHA256SUMS covers every public payload asset", flush=True)


if __name__ == "__main__":
    main()
