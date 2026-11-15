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

## Owned S3 originals checkpoint (November 15)

All 26 selected S3 originals now pass unchanged on both engines against the
pinned, source-built MinIO service. The fixture checks real signatures, versions,
listing, reads, writes and deletes. It preserves virtual-host/path behavior and
maps the original hostnames only inside the test child; no system DNS changes.

HTTP metadata is statement-local by default and database-local when enabled.
Cached versions/regions survive scope changes as upstream; writes invalidate the
selected cache, ETag mismatches permit a fresh retry, and listing metadata avoids
redundant HEAD requests. HTTP logs retain unique case-insensitive headers and the
engine user agent. Literal URL spaces are encoded without changing existing escapes.

The two-connection cache control matches all 23 records on both engines. The prior
24 metadata/secret originals pass again on Rust. Receipts:
`.context/artifacts/popular-extensions/httpfs-s3-verification.json` and
`scripts/httpfs-tests/README.md` (runner instructions).

This brings the retained HTTPFS original inventory to 50/132. The remaining 82
include large datasets, attachments/presigned URLs, HTTP servers, proxies and
public/HuggingFace fixtures. This is owned-service qualification, not live AWS or
final core/platform/package acceptance. No new publication is claimed.

## Download, attachment and public-endpoint checkpoint (November 15)

`force_download` and `force_download_threshold` now use statement-scoped full
downloads. Their storage is buffer-managed, accounts against the memory limit,
and can spill. Range mismatches exhaust retries before fallback; disabled fallback,
warnings and inconsistent HEAD lengths match the oracle controls. Optional HTTP/S3
existence probes follow upstream's failure-to-absence behavior, allowing a
presigned database attachment whose URL cannot authorize a WAL sidecar.

The larger S3 originals exercise paging, partitioned writes, TPC-H/TPC-DS, multipart
uploads, and real presigned files/databases generated by C++ 1.5.6. All eight large
originals pass Rust; seven pass C++. C++ exceeds the unchanged large-upload test's
2.5 GB memory limit on this Mac (14 default threads); that failure remains recorded.
No memory limit, SQL assertion or default thread setting was weakened.

All 17 attachment/local-HTTP/encryption originals pass both engines across their
retained receipts. The repository-error original uses the public endpoint's real
404; denying DNS in the owned fixture produced the wrong prerequisite failure.
Ten download scenarios match results, warnings, errors and request sequences,
normalizing only owned ports and the build-source ID. Eleven native HTTPFS tests,
including bounded-memory spilling and disconnect cleanup, and strict Clippy pass.

The explicit mode audit corrects coverage accounting: five originals disable all
their SQL upstream. One was included in the earlier 50-file raw pass count. The
current retained inventory has **91 active originals passing both engines**, five
upstream-disabled files, and **36 active originals unresolved**. Raw latest SLT
verdicts remain separate: Rust 96 pass/4 fail/3 skip/29 not run; C++ 101 pass/1
fail/1 skip/29 not run. These span multiple candidates and are not a final sweep.

Public-endpoint qualification found remaining connection-cache, filesystem/HTTP-log,
CA diagnostic, glob diagnostic and disabled-filesystem gaps; a CSV service returned
HTTP 500 to both engines. No skip or shared failure is counted as acceptance.
Receipts: `.context/artifacts/popular-extensions/httpfs-download-verification.json`
and `httpfs-originals-audit-1.json`. Final core/platform/package qualification and
publication remain open.
