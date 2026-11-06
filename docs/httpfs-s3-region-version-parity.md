# S3 region, version and remote COPY checkpoint

This is Mac ARM64 development evidence against authenticated C++ DuckDB 1.5.6,
not final release acceptance. The full port goal remains active and incomplete.
Evidence is under `.context/artifacts/platform-matrix-20261029/` on the dedicated
external SSD. Required clients remain Python, Go and Swift, preserving C API/ADBC;
targets remain macOS ARM64, iOS device/simulator ARM64 and Linux ARM64/x86_64.

## Change and independent controls

Signed S3 requests retry once on HTTP 301/400 with a different nonempty
`x-amz-bucket-region`, re-signing the request. The corrected region is retained
with the handle and upload context. Custom endpoints remain fixed; AWS endpoints
are rederived for path and virtual-host styles. Bulk deletion retains C++'s
separate behavior without automatic region retries.

The default-false `s3_version_id_pinning` option captures the version from HEAD
or the metadata fallback GET, then includes its encoded `versionId` in subsequent
GET requests. ETag checking remains enabled and treats one pair of surrounding
quotes as C++ does. Replacement errors retain C++'s diagnostic. No object, table
or query cache was added.

Remote COPY now disables temporary-file output, including explicit
`USE_TMP_FILE=true`, as C++ does. Previously an existing remote target could
select the local temporary-rename path and fail with unsupported S3 rename.
The local COPY branch retains its existing checks and behavior.

The independent loopback receiver verifies SigV4 against each actual method,
path, query, headers and body hash. Its HTTP proxy opens no upstream sockets.
C++ generated two same-length 4,400,448-byte uncompressed Parquet versions with
100,000 rows each; the receiver changes the current version between metadata and
data requests. Results verify sums and row counts, not merely successful opens.
Controls cover encoded version IDs, literal `null`, absent/disabled versions,
unsafe ETag opt-out, equivalent quoted/unquoted ETags, read/list/PUT/multipart
301/400 retries, KMS upload initialization through completion, AWS host changes,
missing/same/repeated region hints and default/explicit remote temporary output.

C++ reference846 and signed Rust849 use the same harness hash:
`3b4ef85361e7ca7cb2896d60563972f364fe3169e6b5a53baf29821e5b538e80`.
All **22 cases / 245 SQL records match literally** after mapping only known
package paths and the actual loopback endpoint. Review863 preserves all records
and request details. The receiver saw 59 C++ requests and 79 Rust requests;
request counts/ranges are reported separately and are not performance acceptance.
The C++ reference runs as Linux AMD64 under Rosetta, not native AMD64 performance.

## Verification on the same development image

Strict workspace/all-target Clippy860 passes. Fresh successful build861 produces
shared library847 and SDK848; normal strict macOS signing and pinned-certificate
whole-file CMS checks pass. Public native exports remain 547 duckdb-prefixed
symbols without private ADBC test hooks. Pipeline862 guards 1,484 code/test inputs
before and after every step. Shared native SHA-256:
`d35cc430d47c9dcec5b2434c2538c7d742b28a2615b0f6e8414b1fe4ce0e9374`.

| Control | Result | Evidence |
| --- | --- | --- |
| New region/version/remote COPY controls | 22 cases, 245 literal records match | C++846, Rust849, review863 |
| TLS and HuggingFace controls | 31 cases, 293 literal records match | TLS851, review864 |
| S3 read/list/limit controls | 30 cases, 276 literal records match | reads854, review865 |
| HTTP controls | 16 cases, 121 literal records match | HTTP855, review865 |
| Upload controls | 35 functional records match | writes853, review866 |
| Partition/delete controls | 30 SQL and 11 SDK records match | partitions852, review867 |
| Extension controls | 59 cases, 246 records unchanged | extensions856, review865 |
| Full public C API suite | 30 files, 155 cases, 751864 assertions pass | CAPI857 |
| Public trust-root metadata controls | 12 literal records match | roots858, review868 |

The upload allocation sample remains 16,515,072 EXTENSION bytes on both engines,
with zero before and after; it is an allocation sample, not peak RSS. Assertion
counts include workload-dependent loops and are not a progress metric. All 21
tracked buffer/cache/batch-memory/Parquet COPY inputs match source830. The accepted
lower-memory policy and coupled speed/RSS acceptance requirements are unchanged.

## Explicit exceptions and remaining gates

- Experiment834 body-only version capture is subsequently corrected in1048/1050.
  The independent suite now has23 cases/256 matching status/row records;255 error
  fields match exactly, with one explicit extra Rust SQL line/caret. Metadata-only
  pinning and replacement detection match C++; see
  `docs/s3-metadata-version-pinning.md`. The original22-case evidence remains
  historical and its failure834 is preserved, not rewritten.
- Experiment835 Location handling is subsequently corrected in1088/1091. Both
  engines pass36 wire cases/405 outcomes and80 method/status/origin/client cases.
  Redirect limits match Curl30/httplib20. Rust deliberately strips cross-origin
  session tokens rather than reproducing C++ forwarding;38 synthetic differences
  remain explicit. See `docs/s3-location-redirect-parity.md`.
- Plain HTTP proxy forwarding is subsequently corrected in1063/1065. Both engines
  pass28 independent cases/314 matching outcomes, including a forwarding-only
  proxy, Basic auth and large method-looking upload. HTTPS retains CONNECT/TLS;
  31 TLS cases pass. Broader proxy configurations remain unverified. See
  `docs/http-forward-proxy-parity.md`; original wire differences remain historical.
- Existing extra listing for Rust prepared EXECUTE, other range/request-count
  differences, credential refresh, broader directory/configuration/options,
  secret transaction/custom-serializer compatibility, platform TLS and real-service
  coverage remain open. The broader HTTPFS goal is not complete.

Failed harness runs834/835/838/839/841/842 and strict850 remain preserved. Run838
exposed missing test secret-directory isolation; the final harness uses private
per-case directories and does not alter user secrets. Strict850 found only the
four-independent-booleans style lint; a scoped annotation precedes successful860.
No behavioral test or expected result was weakened.

Final common-source signed release artifacts still require full SQL-corpus,
shell, C API/ADBC, Python/Go/Swift and platform matrices, cross-engine database/WAL
journeys (including encrypted/torn recovery), and native coupled TPC-H/ClickBench
speed/RSS measurements with individual query outliers and documented acceptance.
The final performance criterion is the accepted lower-memory tradeoff, not exact
C++ speed/memory parity. Mac Intel and additional clients are outside scope.

Persistent key/value file compatibility was subsequently verified in
[secret storage parity](secret-storage-parity.md); broader secret-manager gates
remain open.
