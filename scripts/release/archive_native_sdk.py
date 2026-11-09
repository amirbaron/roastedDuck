#!/usr/bin/env python3
"""Archive and verify every shipping SDK file, excluding private test harnesses."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import tarfile

def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

def verify_archive(archive, provenance):
    assert sha(archive) == provenance["sdk_archive"]["sha256"]
    expected = {name.removeprefix("sdk/"): digest for name, digest in provenance["files_sha256"].items() if name.startswith("sdk/")}
    actual, links = {}, {}
    with tarfile.open(archive) as source:
        for entry in source:
            name = str(PurePosixPath(entry.name))
            assert not PurePosixPath(name).is_absolute() and ".." not in PurePosixPath(name).parts
            if entry.isfile():
                assert name not in actual
                actual[name] = hashlib.file_digest(source.extractfile(entry), "sha256").hexdigest()
            elif entry.issym():
                assert name not in links
                links[name] = entry.linkname
            else:
                assert entry.isdir(), "Unexpected SDK member: " + name
    assert actual == expected, "SDK file set/content mismatch (including CLI, static library, headers and notices)"
    assert links == {"bin/roastedDuck": "rustdb"}
    return {"passed": True, "files_verified": len(actual), "archive_sha256": sha(archive)}

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    version = json.loads((root / "release.json").read_text())["version"]
    provenance = json.loads((args.native / "build-provenance.json").read_text())
    archive = args.out / ("roastedduck-" + version + "-" + provenance["platform"] + ".tar.gz")
    assert not archive.exists()
    with tarfile.open(archive, "w:gz") as output:
        output.add(args.native / "sdk", arcname=".")
    provenance["sdk_archive"] = {"filename": archive.name, "sha256": sha(archive), "bytes": archive.stat().st_size}
    result = verify_archive(archive, provenance)
    (args.native / "build-provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    print(json.dumps(result))
