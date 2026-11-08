#!/usr/bin/env python3
"""Catch inconsistent public release identities and absent legal/doc entrypoints."""
from pathlib import Path
import json
import tomllib

root = Path(__file__).resolve().parents[2]
project = tomllib.loads((root / "python/pyproject.toml").read_text())["project"]
release = json.loads((root / "release.json").read_text())
workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]
assert project["name"] == "roastedduck" and project["version"] == release["version"]
assert release["version"] == release["compatibility_version"] == workspace["version"]
assert project["license"] == "MIT"
for name in ["LICENSE", "README.md", "THIRD_PARTY_NOTICES.md", "SECURITY.md", "CONTRIBUTING.md", "mkdocs.yml"]:
    assert (root / name).is_file(), name
for page in (root / "website/docs").glob("*.md"):
    assert ".context/" not in page.read_text(), f"Private evidence link in {page}"
print("Public metadata and documentation entrypoints verified")
