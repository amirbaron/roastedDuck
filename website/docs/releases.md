# Releases

Install Python from [PyPI](https://pypi.org/project/roastedduck/) with
`python -m pip install roastedduck`. Native SDKs, Swift packages, exact Python
files and verification receipts are on [GitHub Releases](https://github.com/amirbaron/roastedDuck/releases).

## 1.5.6.post3

This release adds roastedDuck's compatible FTS and VSS implementations, offline
installation of the bundled names, and the [Homebrew tap](https://github.com/amirbaron/homebrew-roastedduck).
HNSW remains in-memory only. No C++ extension binary compatibility layer is introduced.

- [Fresh source and distribution qualification](https://github.com/amirbaron/roastedDuck/actions/runs/43584310679).
- [Trusted PyPI publication and all 15 ordinary index installs](https://github.com/amirbaron/roastedDuck/actions/runs/43604870953).
- [Homebrew package/bottle tests](https://github.com/amirbaron/homebrew-roastedduck/actions/runs/43604947328) and [public bottle installs](https://github.com/amirbaron/homebrew-roastedduck/actions/runs/43606039657) on macOS ARM64 and Linux x86_64/ARM64.
- [Exact release files and SHA256SUMS](https://github.com/amirbaron/roastedDuck/releases/tag/v1.5.6.post3).

The workflow's security job initially reported inconsistent terminal metadata.
Its check and aggregate acceptance were rerun successfully without changing the
built native images or Python distributions. Both attempts remain in the workflow history.

All 405 required finite VSS recall cells pass their 0.95 mean threshold; individual
query misses remain explicit. Full local SQL conformance retains 4,801 passes,
76 skips and 20 documented failures with zero ratchet regressions. The five HNSW
persistence files remain deferred. Apple simulator checks and device builds pass;
physical-device execution and notarization remain unverified. Earlier benchmark
results retain their original candidate/date scope.

## 1.5.6.post2

This branding and validation-tooling revision targets DuckDB 1.5.6. Rust crates
and modules use `roastedduck-*` and `roastedduck_*`; the public executable,
interactive banner and build commands use `roastedDuck`. DuckDB-compatible C,
Python and Swift interfaces retain their established names.

All native SDKs, Python distributions and Apple framework slices were built and
qualified against the renamed source. The Apple archive retains its actual build
commit and verifies that its complete engine inputs match the release source.
The source archive includes the corrected validation requirements. Complete resolved test environments are audited alongside
the release's Rust lockfile. Historical benchmark and corpus results retain their
original candidate/date scope.

The [PyPI publication and all 15 ordinary-index installations](https://github.com/amirbaron/roastedDuck/actions/runs/42785009114)
and [three-platform GitHub download checks](https://github.com/amirbaron/roastedDuck/actions/runs/42783959451)
passed. The release contains matching file digests, exact native identities and
raw qualification evidence, including explicit nonpasses.

The [initial installation run](https://github.com/amirbaron/roastedDuck/actions/runs/42783958634)
had four cells resolve the previous post1 release; those failures remain recorded.
The unchanged full rerun linked above verified post2 on all 15 cells.

| Asset | Contents |
| --- | --- |
| Native platform archive | Real `bin/roastedDuck` executable, shared/static libraries, C/ADBC headers, pinned Go source, notices |
| Python wheel / source archive | Source-built distribution; `roastedduck` package, `duckdb` import |
| Swift package archive | Apple XCFramework or extracted/relocated-tested Linux package |
| `SHA256SUMS` | Exact uploaded-file digests |
| `python-distributions.json` | Wheel/source identities and native image hashes |
| `apple-distribution.json` | Signed Apple archive, exact source and runtime evidence |
| `launch-acceptance.json` | Pre-publication artifact acceptance; subsequent publication is verified by the linked install runs |

`main` remains the integration branch and `v1.5-variegata` the maintenance
branch; this revision is tagged `v1.5.6.post2`. Public-preview status remains
explicit. Development signatures are not Apple notarization; physical iOS
execution remains unverified. No crates.io publication is claimed.

## 1.5.6.post1

The [first PyPI release](https://github.com/amirbaron/roastedDuck/releases/tag/v1.5.6.post1)
remains immutable. Its runtime dependency fixes and original installation proofs
are retained. Its source archive contains historical validation pins; developers
should use the current repository tooling to rerun tests.

## Historical previews

The [1.5.6 preview](https://github.com/amirbaron/roastedDuck/releases/tag/v1.5.6)
and [0.1.0-alpha.1 preview](https://github.com/amirbaron/roastedDuck/releases/tag/v0.1.0-alpha.1)
retain their original files and checksums. Their source2551 corpus and performance
results remain dated; use the current security revision for new installations.

See the [changelog](https://github.com/amirbaron/roastedDuck/blob/main/CHANGELOG.md),
[compatibility boundaries](compatibility.md), and [performance evidence](benchmarks.md).
