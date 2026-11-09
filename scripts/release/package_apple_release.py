#!/usr/bin/env python3
"""Package immutable, tested Apple slices with signed evidence and notices."""
import argparse
import datetime
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

from verify_apple_release import CERTIFICATE, ROOT, SLICES, engine_inputs, sha, verify, verify_signature

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--sdk", type=Path, required=True)
parser.add_argument("--evidence", type=Path, required=True, help="JSON mapping evidence filenames to existing receipts/logs")
parser.add_argument("--device-build", type=Path, required=True)
parser.add_argument("--notices", type=Path, required=True)
parser.add_argument("--source-commit", required=True, help="Immutable commit identifying the compiled engine inputs; see recovered build records for checkout timing")
parser.add_argument("--release-commit", required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
assert engine_inputs(args.source_commit) == engine_inputs(args.release_commit)
args.out = args.out.resolve()
args.out.mkdir(parents=True, exist_ok=False)
temporary = args.out / "tmp"
temporary.mkdir()
verify_signature(args.sdk / "SHA256SUMS", args.sdk / "SHA256SUMS.csig", args.sdk / "rustdb-dev.pem", "openssl", temporary, CERTIFICATE)
package = args.out / "apple"
package.mkdir()
shutil.copytree(args.sdk / "DuckDBRust", package / "DuckDBRust", symlinks=True)
shutil.copy2(args.sdk / "rustdb-dev.pem", package / "rustdb-dev.pem")
shutil.copytree(args.notices, package / "DuckDBRust/notices/rust-dependencies")
for name in ["LICENSE", "THIRD_PARTY_NOTICES.md"]:
    shutil.copy2(ROOT / name, package / name)
shutil.copy2(ROOT / "THIRD_PARTY_NOTICES.md", package / "DuckDBRust/THIRD_PARTY_NOTICES.md")
shutil.copy2(ROOT / "LICENSE", package / "DuckDBRust/LICENSE-roastedDuck")
for directory in ["crates", "third_party", "python"]:
    for source in (ROOT / directory).rglob("*"):
        if source.is_file() and source.name.upper().startswith(("LICENSE", "NOTICE", "COPYING")):
            destination = package / "DuckDBRust/notices/project" / source.relative_to(ROOT)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
evidence = package / "evidence"
evidence.mkdir()
for name, source in json.loads(args.evidence.read_text()).items():
    assert Path(name).name == name and name.endswith((".json", ".jsonl", ".log"))
    shutil.copy2(source, evidence / name)
shutil.copy2(args.sdk / "preflight.json", evidence / "sdk-preflight.json")
shutil.copy2(args.sdk / "summary.json", evidence / "sdk-assembly.json")
device = args.device_build.resolve()
verify_signature(device / "SHA256SUMS", device / "SHA256SUMS.csig", device / "rustdb-dev.pem", "openssl", temporary, CERTIFICATE)
device_report = json.loads((device / "summary.json").read_text())
assert sha(device / "summary.json") == sha(evidence / "device-build.json")
product = Path(device_report["executable"]).parent / "Cduckdb.framework/Cduckdb"
selected = package / "DuckDBRust/Artifacts/Cduckdb.xcframework/ios-arm64/Cduckdb.framework/Cduckdb"
assert sha(product) == sha(selected)
assert sha(Path(device_report["executable"])) == device_report["executable_sha256"]
subprocess.run(["codesign", "--verify", "--strict", str(product)], check=True)
sidecar = {"observed_after_build": True, "runtime_verified": False,
           "observed_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
           "selected_framework_sha256": sha(product), "consumer_executable_sha256": device_report["executable_sha256"],
           "device_build_receipt_sha256": sha(evidence / "device-build.json"),
           "original_signed_consumer_manifest_sha256": sha(device / "SHA256SUMS"),
           "original_signed_sdk_manifest_sha256": sha(args.sdk / "SHA256SUMS"),
           "scope": "Post-build identity check of retained immutable link inputs; no physical-device execution"}
(evidence / "device-input-identity.json").write_text(json.dumps(sidecar, indent=2) + "\n")
images = {platform: sha(package / "DuckDBRust/Artifacts/Cduckdb.xcframework" / relative)
          for platform, (relative, _) in SLICES.items()}
release = json.loads((ROOT / "release.json").read_text())
metadata = {"version": release["version"], "compatibility_version": release["compatibility_version"],
            "engine_source_identity_commit": args.source_commit, "assembly_commit": args.release_commit,
            "engine_inputs": engine_inputs(args.release_commit), "native_images": images,
            "physical_device_verified": False, "profile": "repository release profile",
            "source_equivalence_scope": "Engine, dependency lockfile, toolchain pin, build configuration, Swift C header and Swift sources; source identity is not a claim about the checkout commit or byte-reproducibility. See recovered invocation evidence."}
(package / "apple-release.json").write_text(json.dumps(metadata, indent=2) + "\n")
(package / "README.md").write_text("# roastedDuck Apple SDK\n\nDuckDB 1.5.6 compatibility, security revision " + release["version"] + ".\n\n"
    "Use the Swift package in `DuckDBRust`. The archive includes macOS arm64, iOS arm64, and iOS Simulator arm64 slices. "
    "Mac and simulator runtime evidence is in `evidence`; physical iOS execution remains unverified. "
    "The development signature is self-signed and is not Apple notarization.\n\n"
    "Documentation: https://amirbaron.github.io/roastedDuck/swift/\n")
sdk_summary = json.loads((args.sdk / "summary.json").read_text())
sdk_summary["files"] = {str(path.relative_to(package / "DuckDBRust")): sha(path)
                        for path in (package / "DuckDBRust").rglob("*") if path.is_file() and not path.is_symlink()}
sdk_summary["release_version"] = release["version"]
(package / "summary.json").write_text(json.dumps(sdk_summary, indent=2) + "\n")
shutil.copy2(args.sdk / "preflight.json", package / "preflight.json")
sdk_links = {str(path.relative_to(package / "DuckDBRust")): str(path.readlink())
             for path in (package / "DuckDBRust").rglob("*") if path.is_symlink()}
(package / "sdk-symlinks.json").write_text(json.dumps(sdk_links, indent=2) + "\n")
links = {str(path.relative_to(package)): str(path.readlink()) for path in package.rglob("*") if path.is_symlink()}
(package / "symlinks.json").write_text(json.dumps(links, indent=2) + "\n")
files = sorted(path for path in package.rglob("*") if path.is_file() and not path.is_symlink())
(package / "SHA256SUMS").write_text("".join(sha(path) + "  " + str(path.relative_to(package)) + "\n" for path in files))
subprocess.run(["codesign", "--force", "--sign", "rustdb-dev", "--timestamp=none", "--identifier", "dev.roastedduck.AppleReleaseManifest",
                "--detached", str(package / "SHA256SUMS.csig"), str(package / "SHA256SUMS")], check=True)
result = verify(package, args.release_commit, temporary)
archive = args.out / ("roastedduck-" + release["version"] + "-swift-apple.tar.gz")
with tarfile.open(archive, "w:gz") as output:
    output.add(package, arcname=".")
result.update(filename=archive.name, sha256=sha(archive), bytes=archive.stat().st_size, version=release["version"])
(args.out / "apple-distribution.json").write_text(json.dumps(result, indent=2) + "\n")
print("Packaged", archive.name, "with signed payloads and runtime evidence")
