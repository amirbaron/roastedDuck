"""Apply recorded production-client adaptations to a freshly verified pin."""

import difflib
import hashlib
from pathlib import Path
import subprocess


def apply_source_adaptations(package):
    workspace = Path(__file__).resolve().parents[2]
    patch = workspace / "swift/patches/linux-decimal-public-api.patch"
    names = [line[6:] for line in patch.read_text().splitlines() if line.startswith("--- a/")]
    originals = {name: (package / name).read_text() for name in names}
    subprocess.run(["patch", "--batch", "--forward", "-p1", "-i", str(patch)],
                   cwd=package, check=True, capture_output=True, text=True)
    adaptations = []
    for name, original in originals.items():
        patched = (package / name).read_text()
        adaptations.append({
            "path": name,
            "original_sha256": hashlib.sha256(original.encode()).hexdigest(),
            "patched_sha256": hashlib.sha256(patched.encode()).hexdigest(),
            "patch_sha256": hashlib.sha256(patch.read_bytes()).hexdigest(),
            "reason": "Linux Decimal public APIs; Darwin code and upstream tests unchanged",
        })
    (package / "source-adaptations.diff").write_text("".join(
        line for name, original in originals.items()
        for line in difflib.unified_diff(original.splitlines(True),
                                        (package / name).read_text().splitlines(True),
                                        fromfile="upstream/" + name, tofile="adapted/" + name)))
    return adaptations
