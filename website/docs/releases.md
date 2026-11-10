# Releases

Install Python from [PyPI](https://pypi.org/project/roastedduck/) with
`python -m pip install roastedduck`. Native SDKs, Swift packages, exact Python
files and verification receipts are on [GitHub Releases](https://github.com/amirbaron/roastedDuck/releases).

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
