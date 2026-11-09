#!/usr/bin/env python3
"""Stage newly compiled native SDK inputs; never reuse an old engine image."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--target", required=True)
parser.add_argument("--platform", required=True)
parser.add_argument("--out", type=Path, required=True)
parser.add_argument("--notices", type=Path, required=True)
parser.add_argument("--harnesses", type=Path, required=True)
parser.add_argument("--go-source", type=Path, required=True)
args = parser.parse_args()
compiled = ROOT / "target" / args.target / "release"
args.out.mkdir(parents=True, exist_ok=False)
sdk = args.out / "sdk"
(sdk / "bin").mkdir(parents=True)
(sdk / "lib").mkdir()
shutil.copytree(ROOT / "include", sdk / "include")
(sdk / "adbc").mkdir()
shutil.copy2(ROOT / "include/adbc.h", sdk / "adbc/adbc.h")
for notice in ["LICENSE-APACHE", "NOTICE"]:
    shutil.copy2(ROOT / "crates/capi/src/adbc" / notice, sdk / "adbc" / notice)
go_pin = json.loads((ROOT / "go/upstream.json").read_text())
go_files = {str(path.relative_to(args.go_source)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in args.go_source.rglob("*") if path.is_file()}
assert go_files == go_pin["files"]
shutil.copytree(args.go_source, sdk / "go/duckdb-go")
shutil.copy2(compiled / "roastedDuck", sdk / "bin/roastedDuck")
library = "libduckdb.dylib" if args.platform == "macos-arm64" else "libduckdb.so"
for name in [library, "libduckdb.a"]:
    shutil.copy2(compiled / name, sdk / "lib" / name)
if args.platform == "macos-arm64":
    subprocess.run(["install_name_tool", "-id", "@rpath/libduckdb.dylib", str(sdk / "lib" / library)], check=True)
    for path in [sdk / "lib" / library, sdk / "bin/roastedDuck"]:
        subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(path)], check=True)
        subprocess.run(["codesign", "--verify", "--strict", str(path)], check=True)
    symbols = subprocess.check_output(["nm", "-gU", str(sdk / "lib" / library)], text=True)
    exported = sorted(line.split()[-1].removeprefix("_") for line in symbols.splitlines() if line.split())
else:
    symbols = subprocess.check_output(["nm", "-D", "--defined-only", str(sdk / "lib" / library)], text=True)
    exported = sorted(line.split()[-1] for line in symbols.splitlines() if line.split())
assert len(exported) == 547, (len(exported), exported)
assert not any("test" in name.lower() and any(prefix in name.lower() for prefix in ["duckdb", "roastedduck"]) for name in exported)
(args.out / "exports.txt").write_text("\n".join(exported) + "\n")
for name in ["LICENSE", "THIRD_PARTY_NOTICES.md"]:
    shutil.copy2(ROOT / name, sdk / name)
shutil.copy2(ROOT / "scripts/release/PACKAGE-README.md", sdk / "README.md")
shutil.copytree(args.notices, sdk / "notices/rust-dependencies")
for base in [ROOT / "crates", ROOT / "third_party", ROOT / "python"]:
    for path in base.rglob("*"):
        if path.is_file() and any(marker in path.name.upper() for marker in ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE")):
            destination = sdk / "notices/project" / path.relative_to(ROOT)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, destination)
tools = args.out / "tools"
tools.mkdir()
shutil.copy2(compiled / "slt", tools / "slt")
shutil.copytree(args.harnesses, args.out / "harnesses")
files = {}
for path in sorted(args.out.rglob("*")):
    if path.is_file() and not path.is_symlink():
        with path.open("rb") as stream:
            files[str(path.relative_to(args.out))] = hashlib.file_digest(stream, "sha256").hexdigest()
(args.out / "build-provenance.json").write_text(json.dumps({
    "platform": args.platform, "target": args.target, "sdk_layout": "roastedduck-v2",
    "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
    "export_count": len(exported), "adbc_test_api": False, "files_sha256": files,
    "macos_signing": "ad-hoc CI development signature; not notarized" if args.platform == "macos-arm64" else None,
}, indent=2) + "\n")
print("Staged", args.platform, "with", len(exported), "public exports")
