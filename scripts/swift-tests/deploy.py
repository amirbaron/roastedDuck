"""Copy a verified Swift consumer and framework away from SwiftPM and run it."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess

from prepare import digest
from package_identity import verify_manifest


def main():
    workspace = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--consumer-run", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    source, out = args.consumer_run.resolve(), args.out.resolve()
    if out.exists():
        parser.error("preserve evidence: output must be new")
    report = json.loads((source / "summary.json").read_text())
    fingerprint = report.get("sdk_certificate_sha256")
    if fingerprint:
        verify_manifest(source, fingerprint, out.parent)
    executable = Path(report.get("executable", str(source / "build/debug/Consumer")))
    assert digest(executable) == report["executable_sha256"]
    image = Path(report["consumer"]["function_image"])
    assert digest(image) == report["loaded_framework_sha256"]
    framework = next(parent for parent in image.parents if parent.name == "Cduckdb.framework")
    out.mkdir(parents=True)
    bundle = out / "distribution"
    bundle.mkdir()
    shutil.copy2(executable, bundle / "Consumer")
    shutil.copytree(framework, bundle / framework.name, symlinks=True)
    shutil.copy2(source / "relocated/DuckDBRust/LICENSE", bundle / "LICENSE")
    for artifact in [bundle / "Consumer", bundle / framework.name]:
        subprocess.run(["codesign", "--verify", "--strict", str(artifact)], check=True)
    if fingerprint:
        shutil.copy2(source / "rustdb-dev.pem", bundle / "rustdb-dev.pem")
        links = {str(p.relative_to(bundle)): os.readlink(p) for p in bundle.rglob("*") if p.is_symlink()}
        assert all((bundle / name).resolve().is_relative_to(bundle) for name in links)
        (bundle / "symlinks.json").write_text(json.dumps(links, indent=2) + "\n")
        manifest = bundle / "SHA256SUMS"
        payloads = sorted(p for p in bundle.rglob("*") if p.is_file() and not p.is_symlink())
        manifest.write_text("".join(digest(p) + "  " + p.relative_to(bundle).as_posix() + "\n" for p in payloads))
        subprocess.run(["codesign", "--force", "--sign", "rustdb-dev", "--timestamp=none",
                        "--identifier", "dev.rustdb.SwiftConsumerDistributionManifest", "--detached",
                        str(bundle / "SHA256SUMS.csig"), str(manifest)], check=True)
        verify_manifest(bundle, fingerprint, out.parent)
    for name in ["fixtures", "tmp", "foundation-home"]:
        (out / name).mkdir()
    env = dict(os.environ, TMPDIR=str(out / "tmp"), CFFIXED_USER_HOME=str(out / "foundation-home"))
    with (out / "run.log").open("x") as log:
        result = subprocess.run(["nice", "-n", "10", str(bundle / "Consumer"), str(out / "fixtures")],
                                env=env, stdout=log, stderr=subprocess.STDOUT, timeout=30)
    (out / "terminal.json").write_text(json.dumps({"exit_code": result.returncode}) + "\n")
    assert result.returncode == 0
    actual = json.loads((out / "fixtures/consumer.json").read_text())
    assert Path(actual["function_image"]).is_relative_to(bundle)
    assert actual["engine_images"] == [actual["function_image"]]
    assert digest(Path(actual["function_image"])) == report["loaded_framework_sha256"]
    assert digest(bundle / "Consumer") == report["executable_sha256"]
    if fingerprint:
        verify_manifest(bundle, fingerprint, out / "tmp")
        verify_manifest(source, fingerprint, out / "tmp")
        assert {str(p.relative_to(bundle)): os.readlink(p) for p in bundle.rglob("*") if p.is_symlink()} == links
    summary = {"consumer": actual, "executable_sha256": digest(bundle / "Consumer"),
               "framework_sha256": digest(Path(actual["function_image"])),
               "distribution": str(bundle), "exit_code": result.returncode,
               "architecture": report.get("architecture", "arm64"),
               "scope": "Standalone macOS consumer/framework relocation on current host"}
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
