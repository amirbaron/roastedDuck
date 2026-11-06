# S3 partitioned COPY and deletion checkpoint

2026-10-31. Historical partition/delete checkpoint731; see [the newer HF checkpoint](httpfs-huggingface-parity.md). The complete release goal remains open.

S3 now reports virtual directories and remote-file semantics. Directory COPY
therefore skips the local file-versus-directory check and avoids creating virtual
partition directories. The registered filesystem interface provides default
recursive listing and multi-file removal for existing backends; S3 overrides
these to enumerate object keys without extra per-key HEAD requests and submit
signed bulk deletion requests.

Single-object deletion opens the object, sends DELETE and preserves the pinned
C++ missing/denied/disabled-filesystem diagnostics, including errno metadata.
Bulk deletion splits requests at 1,000 keys, signs the body hash, Content-MD5 and
XML content type, and checks HTTP status plus embedded per-object errors. Batches
remain separated by endpoint, credentials, HTTP parameters and selected secret
identities, even when two scoped secrets contain identical credential values.
The implementation follows HTTPFS commit
`4bc690dba4496c765777a0269d48fdbaff7cdc11`.

Partitioned CSV/Parquet COPY, overwrite-or-ignore, overwrite and append now work
through this boundary. The controls cover directory non-emptiness, partition
schema inference, NULL and encoded values, obsolete-object removal and an object
whose name is also a prefix. This corrects signed before-image685's first
partitioned COPY failure: “S3 directories are not supported” (before701).

No object-body, table or query cache or memory-budget increase was introduced.
Existing `release_on_unpin` behavior is unchanged. The earlier distinction of one
additional listing request per Rust prepared EXECUTE is still recorded; no cache
was introduced to remove it.

## Verification

Evidence root: `.context/artifacts/platform-matrix-20261029/`, on this workspace's
dedicated external SSD. Aggregate comparison: `s3-partition-review-730`.

| Check | Result | Evidence |
| --- | --- | --- |
| Partition/overwrite/append/bulk/error SQL | 30 literal records match C++ | C++729, Rust727 |
| SDK single removal/missing/denied/disabled/database-valid | 11 literal records match C++ | signed C++710 and corrected Rust726, comparison730 |
| Bulk deletion contents | Identical key batches; 1,005 keys split 1,000+5; distinct scoped secret isolated | comparison730 |
| Existing S3 upload controls | 35 functional records match; allocation sample remains 16515072 bytes each, zero before/after | writes717, review723 |
| Existing read/list/limit controls | 30 cases, 276 literal records match | reads718, review722 |
| Existing HTTP/TLS/ETag controls | 16 cases, 121 literal records match | HTTP719, review722 |
| Existing extension controls | 59 cases, 246 records unchanged | integrated720 |
| Full public C API suite | 30 files, 155 cases, 775682 assertions pass | fullCAPI721 |
| Strict workspace/all-target Clippy | Pass | check728 |

The independent on-disk receiver verifies SigV4, actual payload hashes and the
bulk request's Content-MD5. It validates manifest key counts, services paginated
listing and retains object visibility on denied deletion. Tests use synthetic
credentials and loopback HTTP. The single-delete drivers are separate SDK
adapters; they are not shipping functions. Their scalar operations are volatile
and execute with the real query's filesystem context. The Rust SDK uses an
isolated secret directory, and SDK child processes exclude ambient cloud/proxy
credentials.

Only known package paths and actual loopback endpoints are mapped in the new
comparison. SQL, statuses, error text and rows compare literally. Bulk request
order is not required equal because C++ iterates an unordered batch map; batch
contents are checked independently. Append filenames are intentionally random;
the controls assert resulting key counts and query values rather than rewriting
random filenames into comparable values. No tests, skips or ratchets were weakened.

The production shared image714 was freshly built713, strictly codesigned and
whole-file CMS authenticated before/after runtime. Native SHA-256:
`191fbfb115d9f0f2b59ab9f6ad681dc12424d692205a8bc22df8be2d93aba68f`.
It retains 547 duckdb-prefixed exports and excludes private ADBC test hooks.

Rust SDK726 was separately rebuilt725 and authenticated. Only its test adapter
changed after build713: the first probe715 omitted allocation of its Boolean
result vector and panicked after a successful deletion. The corrected probe
allocates that vector. The root library remained cached and byte-identical to
build713's original native artifact; continuation727 preserves that check. This
is honest development cache reuse, not a claim of a new production rebuild.

The C++ SDK oracle710 is a separately compiled and CMS-authenticated Linux AMD64
client linked against the pinned reference engine. Oracle705/708 initially passed
an explicit opener to C++'s client opener filesystem and hit its assertion;
build709 corrects the adapter. Fixture699 initially used hierarchical local object
paths and could not represent S3's object/prefix coexistence; the corrected receiver
stores payloads under opaque hashes. These failures, the earlier style lint702,
probe716 and the before701 failure remain preserved alongside successful receipts.

## Remaining gates

Full directory-operation/API coverage, remaining upload and batching configuration/
protocol/XML/secret combinations, region redirects, credential refresh, version
pinning and real service interoperability remain open. These controls use an
independent receiver, not AWS or MinIO. HuggingFace, remaining HTTPFS options,
system trust roots, autoload/startup and other core gaps are unfinished.

Final common-source signed release verification still requires the full SQL and
shell suites, C API/ADBC and Python/Go/Swift matrices, required Linux/iOS targets,
cross-engine DB/WAL journeys and coupled speed/RSS measurements with individual
query outliers and documented acceptance. The allocation sample above is not
peak RSS or performance acceptance. Mac Intel and other language clients remain
outside scope. This checkpoint does not complete S3, HTTPFS or the release goal.
