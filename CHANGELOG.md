# Changelog

## 1.5.6

Align the public release tag, Python distribution, native asset names, and
maintenance branch with the DuckDB 1.5.6 compatibility target. The matching
release branch is `v1.5-variegata`; `main` remains the integration branch.
Preview status is recorded separately from version compatibility.

The native engine bytes and existing compatibility boundaries are unchanged.
Python packaging/version metadata has separate installation checks. This
release supersedes `0.1.0-alpha.1`, which remains available as historical evidence.

## 0.1.0-alpha.1

First roastedDuck public preview, targeting the DuckDB 1.5.6 compatibility contract.

- Rust SQL engine, shell, persistence/WAL, codecs, and HTTP/S3 support.
- Native packages for macOS arm64 and Linux arm64/x86_64.
- CPython 3.10–3.14 wheels; selected Go and Swift clients; C API and ADBC.
- MIT license, public documentation, CI, and downloadable verification evidence.
- New Python distribution identity `roastedduck` (`0.1.0a1`); compatibility imports
  remain `duckdb` and `_duckdb`. Engine version reporting remains separately pinned.

The engine payload originates from the verified November 6 source2551 candidate.
Packaging/branding changes retain native bytes and have separate installation
checks. Consult the release manifest for exact hashes and provenance.

See [compatibility](website/docs/compatibility.md) and
[benchmarks](website/docs/benchmarks.md) for known limitations. Physical iOS
execution is unverified. The preview does not support Windows or Intel Macs.
