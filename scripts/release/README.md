The upcoming candidate is `1.5.6.post3`, adding bundled FTS/VSS and Homebrew
distribution. Publish only after the existing qualification and artifact gates
pass. `render_homebrew.py` renders the tap formula from the accepted release's
SHA256SUMS; it does not publish or invent checksums.

# Release pipeline

The current source candidate is `1.5.6.post2`, a branding and validation-tooling revision
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
The ADBC test bridge is isolated from the 547-export shipping library. A separate
public ADBC 1.0/1.1 consumer uses the shipped header and feature-free library and
checks Arrow schema/batch ownership after releasing the result stream.

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

`archive_native_sdk.py` checks complete shipping inventories, including the CLI,
static libraries, headers and notices. `package_apple_release.py` preserves the
qualified Apple framework bytes, signs their evidence and notices, and records
engine-source equivalence separately from build-checkout timing. The publisher
checks the Apple archive's pinned signature, three ARM slices, full Mac and
simulator runtime receipts, and device build-only identity. It never upgrades
the physical-device exception into a runtime pass.

Native SDKs retain the complete pinned Go client source. Linux Swift packages
are archived, extracted, built into a separate consumer, and relocated before
their exact archive and native hashes are accepted. Their source-build artifacts
and public downloads must agree byte for byte.

An already-uploaded PyPI filename must have the exact accepted digest. Never
replace or silently relabel a previously published native build. Use a post
release revision when a security or packaging fix keeps the compatibility target.

## Verification-only observer

Python 3.10's bundled pip 23 reports its SHA-256 using the legacy `hash` field;
newer pip versions also provide `hashes`. Both forms must identify the same
accepted bytes. Conflicting, absent or non-SHA-256 values are rejected.

When only the aggregate receipt interpreter needs correction, keep the native
producer and release tag frozen. `observe-candidate.yml` uses a separately
identified verification-tools commit and the immutable producer artifact IDs
and ZIP digests pinned through `observer-inputs.json` and GitHub's API. It
requires every original build, security, source rebuild and runtime qualification
job to succeed. The only permitted failed aggregate is the specifically diagnosed
legacy pip `KeyError: 'hashes'`; that failure remains recorded as a failure.

The observer tests its parser, re-evaluates all 15 Python cells and complete native,
crypto, client, archive and signed Apple evidence, and reconstructs a manifest
from untouched distributions. It never rebuilds, re-signs or repacks wheels.
Product/build-input changes require a new producer. The publisher independently
checks both runs, their commits, job IDs, original artifact IDs/digests, raw
receipts and promoted archive bytes. A successful observer does not turn a failed
original workflow into a green one.

Apple input assets are staged in a private draft release solely for transport to
the observer. This creates no official release/tag or publication claim. Public
promotion follows complete observer acceptance; PyPI and live-install checks
remain separate requirements.

### Recovery of unstarted hosted jobs

The frozen producer encountered a GitHub internal server error (correlation ID
`6dd9873b-739a-4cb7-a9a7-5fde0aa6d89a`). All six emitted build/security/source/ARM64
qualification jobs passed; Linux x86_64 and macOS qualification jobs were never
created, and assembly was skipped. GitHub rejected a failed-job retry.

`recover-qualification.yml` runs those two missing jobs against the exact original
artifact IDs and ZIP digests in `recovery-inputs.json`. It checks out the frozen
producer commit and runs its original runtime steps verbatim. The separately
checked-out verification helper only authenticates and downloads the original
inputs. It does not build new engine libraries or Python wheels.

This is a distinct, narrowly bounded observer mode: exactly six original jobs
must remain successful, assembly must remain skipped, and exactly the two missing
jobs must succeed in the pinned recovery run. Any failed original runtime or
build job is rejected. Original and recovery artifacts have separate provenance.
The Linux x86_64 Swift archive is first created by the recovery's unchanged
producer packaging script from the original library, and is explicitly recorded
as a derived recovery artifact. All platform/ABI, image identity, archive,
encryption, and client-suite checks remain mandatory. The original workflow
continues to be reported as failed; recovery does not rewrite its result.

## Maintaining validation dependencies

`validate-published-python.yml` requalifies the immutable 1.5.6.post1 wheels when
validation dependencies change. It verifies release checksums and native hashes,
binds the artifact source to its immutable release tag, and runs all five ABIs on each of the
three supported platforms against the same pinned upstream corpus. It does not
rebuild or repack release files. The corpus fetcher and runtime identity guard
must match the release source. The recorded tooling commit may contain newer
engine or CLI work, but these jobs execute only the installed published images;
they do not qualify those unpublished source changes.

Each C++ environment is resolved first; its exact versions constrain the Rust
environment. Both full `pip inspect` inventories and `pip freeze` records are
retained, and the non-engine package/version maps must match. OSV audits all 30
resolved inventories plus the immutable release tag’s Cargo.lock. `--no-resolve` prevents the scanner from
substituting hypothetical transitive versions for those actually installed; no
installed package is omitted from the audit lockfiles. The build workflow also
checks direct validation pins in addition to its Rust lockfile scan.

The original 1.5.6.post1 Cargo scan covered Rust dependencies. The immutable source
archive retains the validation requirements present at release time, including
the older pytest, PyArrow, and fsspec pins. These are not installed by the
roastedduck distribution. Developers rerunning validation should use the current
repository's corrected requirements and workflow; historical receipts and source
archives are not rewritten.

## Historical preview assembler

`package_preview.py` exists only to reproduce the old source2551 1.5.6 preview
from its frozen input bundle. It refuses new security-revision versions and
changed runtime sources. It is not the PyPI builder. Historical artifacts and
failed receipts remain available with their original scope.
