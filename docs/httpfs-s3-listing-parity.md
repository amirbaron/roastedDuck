# S3 listing and incremental glob checkpoint

2026-10-31. Historical listing checkpoint676; see [the newer write checkpoint](httpfs-s3-write-parity.md). The complete release goal remains open.

S3 listing now uses signed ListObjectsV2 requests, URL-encoded keys and continuation
tokens, component-wise wildcard matching, recursive `**`, directory-marker exclusion,
and query-option propagation. It follows pinned C++ HTTPFS commit
`4bc690dba4496c765777a0269d48fdbaff7cdc11`, including the density-based switch from
flat listing to directory traversal. `s3_allow_recursive_globbing=false` retains
flat listing. No response-body, table or query cache was introduced; existing
memory budgets and `release_on_unpin` remain unchanged.

The byte matcher moved unchanged from the CSV crate into `rustdb_common::glob`.
CSV re-exports it, and S3 uses the same implementation. The matcher body was
verified byte-identical against source645. The local bracket-pattern control
also passes on both engines.

The registered filesystem interface has an optional `GlobResult` page source.
Existing callback filesystems retain their eager fallback. The `glob()` table
function primes up to two paths while binding, then scans available paths without
fetching another page into the same chunk. This follows C++'s `GetExpandResult`
and `FETCH_IF_AVAILABLE` boundaries. Expanded filenames remain in the statement's
file list, as they did before; object contents are not retained there.

This fixed a verified correctness defect in the first listing image655. Rust
initially fetched all pages while binding, so `LIMIT 1` failed when only a later
page returned 403. C++ succeeded. Signed before663 preserves that failure. The
incremental image666 now matches C++ for limits, prepared re-execution, multiple
patterns, and errors reached by complete scans. The late-error SQL caret also
matches without suppressing or rewriting diagnostics. HTTP error XML parsing is
tolerant of truncated bodies; such errors leave the database usable.

## Verified scope

Evidence root: `.context/artifacts/platform-matrix-20261029/`, on this workspace's
dedicated external SSD. Aggregate: `s3-lazy-review-671/verification.json`.

| Check | Result | Evidence |
| --- | --- | --- |
| S3 reads/secrets/listing/limits/error controls | 30 cases, all 276 SQL records match C++ | C++662, signed Rust667 |
| Paginated listing request sequence | 8 requests each, paths/statuses equal | review671 |
| Dense directory traversal | 8 requests each, paths/statuses equal | review671 |
| Forced flat listing | 5 requests each, paths/statuses equal | review671 |
| HTTP/TLS/ETag/secret controls | 16 cases, 121 SQL records match C++ | C++618, Rust668 |
| Existing extension controls | 59 cases, 246 records unchanged | integrated669 |
| Full public C API suite | 30 files, 155 cases, 704444 assertions pass | fullCAPI670 |
| Strict workspace/all-target Clippy | Pass | check664 |

The independent loopback server verifies canonical requests and SigV4 HMAC on
every signed request. It serves paginated objects, common prefixes, encoded keys,
CSV and independent C++ Parquet data. Controls include successful wildcard reads,
empty results, literal paths, deferred URL-option validation, 400/403/404 errors,
truncated error XML, later-page failure and repeated prepared execution.

Only known package/output paths and actual loopback endpoints are mapped in S3
comparisons; encoded endpoints use a separate placeholder. SQL, rows, status and
errors compare literally. Existing HTTP comparisons additionally map known TLS/
probe paths and versioned platform components. Their diagnostic range choices
remain separately recorded, as in the earlier HTTP checkpoint. No test, skip or
ratchet was weakened. Original compile/lint failures and the functional before
image remain preserved.

Image666 was freshly built665, normally codesigned and strictly verified, with
whole-file pinned-certificate CMS authentication before/after runtime. Its native
SHA-256 is
`9d7aa2ccc6477d4475dc77e92ac169fc4f24737967eab5275ca6b5210dbef790`.
It retains 547 duckdb-prefixed exports and excludes private ADBC test hooks.
The pinned C++ HTTPFS packages remain authenticated data selecting the Rust port;
their machine code is never executed by Rust. Generic C++ extension ABI support
is still absent.

## Request-count difference and remaining gates

The limit/prepared group makes 8 listing requests on Rust versus 6 on C++. A
separate telemetry-only probe673/674 identifies the difference: `PREPARE` makes
one request on each engine; each subsequent Rust `EXECUTE` makes one more, while
C++ reuses its statement's expanded paths. All results/errors match. The individual
query counts are preserved in `s3-list-request-disposition-675/verification.json`.
This remains an explicit request-count/performance distinction; this checkpoint
does not accept its performance or add a cache to remove it.

S3 multipart writes/deletion, region redirects, credential refresh, version
pinning, full metadata/XML/URL/protocol coverage and real S3-service interoperability
remain open. These controls use an independent fixture, not AWS or MinIO. The
broader CSV/Parquet reader file-list lifecycles still need their own deferred-I/O
controls; this checkpoint specifically verifies incremental `glob()` scanning and
successful wildcard CSV/Parquet reads. HuggingFace, remaining HTTPFS settings,
system trust roots, autoload/startup and remaining core gaps are also unfinished.

No full selected-client matrix, Linux/iOS runtime refresh, final SQL/shell/ADBC,
cross-engine storage/WAL or coupled speed/RSS acceptance follows from this result.
Required clients remain Python, Go and Swift; required platforms remain Mac ARM64,
iOS ARM64 device/simulator and Linux ARM64/x86_64. Mac Intel is outside scope.
Final common-source signed release verification and documented performance
acceptance are still required.
