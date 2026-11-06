# S3 COPY and upload finalization checkpoint

2026-10-31. Historical write checkpoint696; see [the newer partition/delete checkpoint](httpfs-s3-partition-delete-parity.md). The release goal remains incomplete.

The S3 provider now supports sequential output, the pinned C++ small-file PUT
path, multipart uploads, ordered completion manifests, part retries and explicit
writer finalization. CSV, BLOB, gzip CSV and Parquet COPY reach the registered
filesystem. `s3_uploader_max_filesize`, `s3_uploader_max_parts_per_file` and
`s3_uploader_thread_limit` are registered. Part sizing follows C++'s usable block
size, including its 8-byte header subtraction before rounding.

Output now has a distinct successful close boundary. CSV/BLOB finish compression
and flush their stream before closing; Parquet flushes its footer before closing.
Ordinary flushes do not publish a partial object. FileSync explicitly finalizes an
S3 upload. Completion failures propagate through the query; publication is not
hidden in a destructor. The default close method leaves existing filesystem
callbacks' flush behavior unchanged. Stream adapters preserve the original
callback error type and diagnostic metadata through the existing callback-error
helper, now shared with storage.

Upload parts use `BufferManager::allocate(MemoryTag::Extension, ...)`. Full parts
move into bounded upload workers without copying their payload. Allocation
pressure waits for an in-flight part to release its accounted allocation before
retrying; the configured memory budget is not increased. Worker completion/error
state wakes waiting writers, and handles join their workers before destruction.
No object-body, table or query cache was added. Existing `release_on_unpin` and
global memory policies are unchanged.

Signing includes actual payload hashes, content type and the pinned SSE-KMS
headers for relevant requests. The local receiver checks these headers; it does
not implement KMS encryption or prove access to an AWS KMS service. HTTP400
RequestTimeout and retryable HTTP failures are retried, as are HTTP200 responses
containing an InternalError during multipart completion. Other embedded completion
errors are surfaced. A missing ETag reports an error even if the server already
published a successful PUT, matching the reference behavior.

## Verification

Evidence root: `.context/artifacts/platform-matrix-20261029/`, on this workspace's
dedicated external SSD. `s3-write-review-691/verification.json` records the write
comparison; `s3-write-integrated-review-694/verification.json` records regressions.

| Check | Result | Evidence |
| --- | --- | --- |
| COPY/retry/failure/visibility controls | 35 functional SQL records match C++; one allocation measurement reported separately | C++680, signed Rust686, review691 |
| Blocked-PUT native allocation sample | 16515072 bytes charged to EXTENSION on each engine; zero before/after | review691 |
| Published payloads | CSV, BLOB and gzip bytes match; large CSV 13488894 bytes in three parts | review691 |
| Independent cross-engine payload reads | Four files per direction match, including Parquet | C++692, Rust693, review695 |
| Existing S3 read/list/limit controls | 30 cases, 276 literal records match C++ | Rust687, review694 |
| HTTP/TLS/ETag/secret controls | 16 cases, 121 records match C++ | Rust688, review694 |
| Existing extension controls | 59 cases, 246 records unchanged | integrated689 |
| Full public C API suite | 30 files, 155 cases, 724158 assertions pass | fullCAPI690 |
| Strict workspace/all-target Clippy | Pass | check683 |

The independent receiver stages request bodies and parts on disk, verifies
SigV4 and actual payload hashes, validates part numbers/ETags in the completion
manifest, and publishes the assembled object only on successful completion.
Controls cover single PUT, multipart CSV, gzip/Parquet readback, transient 503 and
400 RequestTimeout, embedded completion retry/error, initialization/part errors,
missing ETag, SSE-KMS headers and object visibility while a PUT is blocked.
All credentials are synthetic and traffic is loopback-only.

The allocation sample comes from a second native connection querying
`duckdb_memory()` while the receiver holds a PUT open. This proves native buffer
accounting and release at that checkpoint. It is not a peak-RSS measurement,
coupled throughput benchmark or final performance acceptance. No budget or cache
was raised to obtain it.

Only known package paths and actual loopback endpoints are mapped in the write
comparison. SQL, status, diagnostics and rows are otherwise compared literally.
The allocation sample remains separately recorded, not normalized. CSV/BLOB/gzip
payloads are byte-identical. The small Parquet files differ (C++ 904 bytes,
Rust 635 bytes), so independent opposite-engine reads verify their actual values
instead of treating a same-engine roundtrip as compatibility proof. This does not
claim identical Parquet encoding or DuckDB database/WAL verification.

Mac ARM64 shared image685 was freshly built684, normally codesigned, strictly
verified, and whole-file CMS authenticated with the pinned certificate before
and after execution. Its native SHA-256 is
`8cc0c0374faa38336b8704582b435cc69dbb2408aa0de203eee476479eee766a`.
It retains 547 duckdb-prefixed exports and excludes private ADBC test hooks. The
HTTPFS package is separately authenticated data selecting the Rust implementation;
its C++ machine code is never executed by Rust.

Original failures remain preserved: compile678's temporary diagnostic borrow,
lint679's documentation error and before682's missing uploader setting. Before682
stopped before COPY; it is not a runtime proof of a COPY failure. Strict681 passed
before the final part-sizing/diagnostic alignment; strict683 and build684 validate
the actual tested implementation. No test, skip or ratchet was weakened.

## Remaining gates

Deletion/bulk deletion and directory/partitioned COPY behavior, region redirects,
credential refresh, version pinning, remaining upload configuration/protocol/error
edges and real S3-service interoperability are still open. This receiver is not
AWS or MinIO. The known extra listing request on each Rust prepared EXECUTE remains
documented in the earlier listing checkpoint and is retained in review694. No cache
was introduced to remove it.

HuggingFace, remaining HTTPFS options, system trust roots, autoload/startup and
other core gaps are unfinished. The full selected-client/ABI/platform matrix,
final SQL/shell/C API/ADBC, cross-engine DB/WAL journeys and coupled speed/RSS
measurements still require common-source signed release artifacts. Required
clients remain Python, Go and Swift; targets remain macOS ARM64, iOS ARM64 device/
simulator and Linux ARM64/x86_64. Mac Intel and other clients remain outside scope.
