# Releases

Install Python from [PyPI](https://pypi.org/project/roastedduck/) with
`python -m pip install roastedduck`. Native SDKs, Swift packages, exact Python
files and verification receipts are on [GitHub Releases](https://github.com/amirbaron/roastedDuck/releases).

## 1.5.6.post1

This security and packaging revision targets DuckDB 1.5.6. It updates PyO3 to
0.30.1, rsa to 0.10.0, and ctutils to 0.4.8, and builds new native images from the
fixed dependency graph. The prior 1.5.6 assets remain immutable.

Qualification covers all 15 normal CPython/platform wheels, the extracted source
archive, native C API/ADBC, Go/Swift, and encrypted checkpoint/WAL interoperability.
The official-index matrix verifies ordinary pip resolution and installed native
hashes on every advertised cell. Matching nonpasses remain recorded.

The [PyPI publication and 15-cell installation run](https://github.com/amirbaron/roastedDuck/actions/runs/42691444784)
and [GitHub-download installation run](https://github.com/amirbaron/roastedDuck/actions/runs/42691434280)
both passed. All 16 files on PyPI have the same SHA-256 digests as the accepted
GitHub release distributions.

The immutable source archive preserves its original developer-test requirements.
For rerunning validation, use the [current repository tooling](https://github.com/amirbaron/roastedDuck/blob/main/scripts/release/README.md#maintaining-validation-dependencies),
which updates pytest, PyArrow, and fsspec and audits the resolved test environments.
Those validation packages are not installed by `pip install roastedduck`.

| Asset | Contents |
| --- | --- |
| Native platform archive | CLI, shared/static libraries, C/ADBC headers, pinned Go source, notices |
| Python wheel / source archive | Source-built distribution; `roastedduck` package, `duckdb` import |
| Swift package archive | Apple XCFramework or extracted/relocated-tested Linux package |
| `SHA256SUMS` | Exact uploaded-file digests |
| `python-distributions.json` | Wheel/source identities and native image hashes |
| `apple-distribution.json` | Signed Apple archive, source-equivalence and runtime evidence |
| `launch-acceptance.json` | Pre-publication artifact acceptance; subsequent publication is verified by the linked install runs |

`main` remains the integration branch and `v1.5-variegata` the maintenance
branch; this revision is tagged `v1.5.6.post1`. Public-preview status remains
explicit. Development signatures are not Apple notarization; physical iOS
execution remains unverified. No crates.io publication is claimed.

## Historical previews

The [1.5.6 preview](https://github.com/amirbaron/roastedDuck/releases/tag/v1.5.6)
and [0.1.0-alpha.1 preview](https://github.com/amirbaron/roastedDuck/releases/tag/v0.1.0-alpha.1)
retain their original files and checksums. Their source2551 corpus and performance
results remain dated; use the current security revision for new installations.

See the [changelog](https://github.com/amirbaron/roastedDuck/blob/main/CHANGELOG.md),
[compatibility boundaries](compatibility.md), and [performance evidence](benchmarks.md).
