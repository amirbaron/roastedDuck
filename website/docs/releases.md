# Releases

Download the preview from
[GitHub Releases](https://github.com/amirbaron/roastedDuck/releases).

## 0.1.0-alpha.1

The first roastedDuck preview targets the DuckDB 1.5.6 compatibility contract.
Its engine payload is the recorded November 6, 2026 source2551 candidate;
public packaging adds project identity, notices, documentation, and installation
checks. The Python distribution version is `0.1.0a1`.

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
