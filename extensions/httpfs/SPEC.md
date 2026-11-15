# httpfs: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-httpfs/tree/4bc690dba4496c765777a0269d48fdbaff7cdc11

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/httpfs.cmake`.
The adjacent `upstream.json` inventories 132 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

HTTP(S) positional reads, S3-compatible read/write/list/glob/delete and multipart commit, Hugging Face access; all options, secrets and table functions registered by the pinned source. Preserve existing Rust transports. Ordinary INSTALL/LOAD becomes bundled and offline; explicit legacy artifact loading retains metadata/signature validation and never executes C++.

## Acceptance

Run unchanged upstream SQL against C++ DuckDB 1.5.6 and Rust. Keep missing-service
prerequisites explicit; skips do not qualify a feature. Add differential controls
for auth, scope, cancellation, transactions and errors where applicable. Use local emulator/disposable-service
integration in addition to protocol fixtures, as authorized by the owner on
November 14. Record live-cloud behavior separately as unverified until exercised. Run scripts/conformance.sh
after integration merges, then verify normal Python and Homebrew packages.
No new C/C++ dependencies, binary adapters, default-off gates or public plugin ABI.

## Status

Contract recorded; not a claim that new functionality passes acceptance.

## Bundled-loading source milestone

The normal build now links HTTPFS and acknowledges bare INSTALL/FORCE INSTALL
offline. LOAD aliases share one initialization; explicit package loading still
validates the pinned legacy package and activates Rust code. A filesystem
autoload callback respects the existing autoload setting (default false) and
rechecks permissions. Explicit repository/version replacement is rejected for
this bundled implementation, as for FTS/VSS.

Final CLI developer/transport checks: 10 PASS (8 Rust, 2 C++ controls), including
CSV, Parquet, aliases, no-reset reloads and zero-request denied-access cases.
Strict all-target Clippy passes; targeted conformance has zero regressions.
Receipts: `.context/artifacts/popular-extensions/bundled-final/` and
`conformance-final/`. This does not qualify all 132 upstream HTTPFS files or
real-cloud interoperability, and is not yet included in a public package.

## Metadata/secret originals checkpoint (November 15)

All24 selected original metadata/secret files pass unchanged on both engines,
including authentic historical serialized secrets. HTTP DNS/timeout errors now
retain the method, URL and selected transport's reason. Secret option binding
uses the underlying cast diagnostic, and local DROP storage names fold case.
The owned HuggingFace fixture supplies a real fake-token cache file.

Eight paired error controls,24 transaction cases/412 public C API records and9
native HTTPFS tests pass; strict Clippy4 passes. Receipts are in
`.context/artifacts/popular-extensions/httpfs-metadata-verification.json`.
The other108 originals still require service fixtures; this is not full HTTPFS
or package/release acceptance. Pinned MinIO/mc source preparation is recorded in
`.context/artifacts/popular-extensions/httpfs-services/source-pins.json`.
