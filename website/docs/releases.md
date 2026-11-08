# Releases

Download the preview from
[GitHub Releases](https://github.com/amirbaron/roastedDuck/releases).

## 1.5.6

The first roastedDuck preview targets the DuckDB 1.5.6 compatibility contract.
Its engine payload is the recorded November 6, 2026 source2551 candidate;
public packaging adds project identity, notices, documentation, and installation
checks. The Python distribution version is `1.5.6`.

Versions and version-family branch names mirror the compatibility target:
`main` for integration, `v1.5-variegata` for maintenance, and the `v1.5.6` tag.
The release is still marked as a preview; a matching version is not a claim
that every feature or platform is equivalent to upstream.

| Asset | Contents |
| --- | --- |
| Native platform archive | CLI, dynamic library, C headers, pinned Go source, notices |
| Static platform archive | Optional static library and notices |
| Python wheel | One CPython/platform combination; `roastedduck` distribution, `duckdb` import |
| Swift package archive | Apple XCFramework or Linux package, sources and notices |
| `SHA256SUMS` | SHA-256 for every uploaded payload asset |
| `provenance.json` | Original candidate identity, source commit, packaging changes, native-byte checks |

Checksums and reproducible provenance are distinct from platform trust.
Historical macOS development signatures are not Developer ID notarization.
The preview is not published to PyPI or crates.io; use these assets or build
from source. Installation checks are separate from the historical full-suite
qualification.

See the [changelog](https://github.com/amirbaron/roastedDuck/blob/main/CHANGELOG.md),
[compatibility boundaries](compatibility.md), and [performance evidence](benchmarks.md).

## Superseded initial preview

The initial [0.1.0-alpha.1 release](https://github.com/amirbaron/roastedDuck/releases/tag/v0.1.0-alpha.1)
is retained with its original assets and checksums. Use 1.5.6 for new installations.
Renumbering changes packaging metadata; it does not rebuild the native engine.
