# Release pipeline

The current source candidate is `1.5.6.post1`, a security and packaging revision
of the DuckDB 1.5.6 compatibility target. Public API compatibility version
identifiers remain 1.5.6.

## Build and qualify

`.github/workflows/build-candidate.yml` builds from a recorded Git commit and
locked dependency graph. It scans dependencies, builds native SDKs and the 15
normal CPython/platform wheels, executes security/persistence harnesses on native
targets, compares the pinned Python fast suite with the official C++ control,
and rebuilds an extracted source archive outside the checkout. Each native target
also runs the full public C API (155 cases), isolated ADBC (64 cases), Go (652
cases), and Swift (132 upstream cases plus one image-identity case) suites.
The ADBC test bridge is isolated from the 547-export shipping library.

`inspect_distribution.py` validates the actual metadata, README, URLs, platform/ABI
tags, native architecture, wheel RECORD, and source closure prerequisites.
`complete_wheels.py` adds third-party notices without changing native entries.
`verify_native_inputs.py` verifies the producer's source identity and complete
native file set before execution. `verify_qualification.py` reconciles every
platform and ABI with producer manifests, installation hashes, in-process image
identities, paired test outcomes, and all four encrypted-storage profiles.
The publisher repeats this reconciliation using the source workflow's raw
artifacts; acceptance booleans alone cannot authorize publication.

## Freeze and promote

Complete Apple simulator and final relocation requirements against the frozen
images. Preserve raw failures and explicit platform exceptions.

Publish a GitHub release containing the exact tested distributions, checksums,
producer manifest and an evidence-backed `launch-acceptance.json`. That manifest
must name the successful source/qualification runs and the actual final native
image hashes for each gate. It must not be synthesized from build success alone.

`.github/workflows/pypi-publish.yml` verifies that acceptance record, producer
workflow identity and distribution hashes, then publishes the same wheels and
source archive using the PyPI trusted publisher for `amirbaron/roastedDuck`,
workflow `pypi-publish.yml`, environment `pypi`. No long-lived token is needed.
The workflow checks ordinary PyPI resolution and installed native hashes on all
15 advertised cells. Live install documentation changes only after those pass.

An already-uploaded PyPI filename must have the exact accepted digest. Never
replace or silently relabel a previously published native build. Use a post
release revision when a security or packaging fix keeps the compatibility target.

## Historical preview assembler

`package_preview.py` exists only to reproduce the old source2551 1.5.6 preview
from its frozen input bundle. It refuses new security-revision versions and
changed runtime sources. It is not the PyPI builder. Historical artifacts and
failed receipts remain available with their original scope.
