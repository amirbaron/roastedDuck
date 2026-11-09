# Changelog

## Unreleased — validation tooling

- Update pytest, PyArrow, and fsspec test dependencies to patched releases and pin current pip/setuptools bootstrap tools.
- Recheck published wheel/native hashes and paired Python outcomes without rebuilding release payloads; retain and audit complete resolved validation environments.
- The immutable 1.5.6.post1 source archive retains historical test requirements. Use current repository tooling for developer validation; these packages are not mandatory runtime dependencies of roastedduck.

## 1.5.6.post1 — November 9, 2026

- Source-built PyPI packages with complete metadata, README, licenses and tested source archives.
- PyO3 0.30.1, rsa 0.10.0 and ctutils 0.4.8 address the identified dependency advisories.
- Explicit GIL requirement preserves normal-CPython scope during the PyO3 upgrade.
- Fresh native security/persistence, C API/ADBC, Go, Swift, and 15-cell Python qualification; historical results stay dated.
- Published to PyPI through trusted publishing; all 15 ordinary official-index installations passed, with installed native hashes, SQL, and encryption verified.
- Native SDKs include shared/static libraries, public C/ADBC headers, pinned Go sources, and complete notices. Linux Swift archives are verified after extraction and relocation.
- Original producer, recovery after a GitHub internal error, and successful observer results remain separately recorded in the release evidence.

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
