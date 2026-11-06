# S3 version pinning from initial metadata

The port previously captured x-amz-version-id on later range reads. In the
body-only case, the initial HEAD has no version; the first body read returns a
version and the object then changes. Rust pinned the old version for subsequent
reads and returned the old data. C++ did not establish a pin from that body header
and reported the ETag replacement. This was the recorded exception834.

The fix retains version capture only in initial metadata processing: HEAD or its
GET fallback. Later data-range responses neither establish nor change the pin.
No cache, memory limit, COPY throttle or query policy changes.

## Independent controls and verified candidate

The existing22-case independent S3 receiver gains pin-body-only. It signs/checks
requests independently, returns a version only on GET, replaces the underlying
object after that response, and requires the ETag replacement failure with no
versionId query on any request. Existing HEAD, fallback-GET, null/absent/disabled
pinning, ETag quoting, region redirects, listing, PUT/multipart, proxy and remote
COPY controls remain intact.

Corrected reference1045 passes23 cases/256 records on native C++934 plus official
HTTPFS590. The prior signed Rust1031 reproduces the discrepancy: its body-only
query succeeds with the old-object rows instead of reporting replacement. That
failure is preserved. Initial wrapper1044 failed on an artifact output-path setup
before native execution; the corrected wrapper is separately retained.

Strict workspace/all-target Clippy1046 and fresh native1047 pass before shared1048
and SQL1049 are signed/authenticated. Corrected Rust1050 passes all23 cases. Audit
1052 verifies all256 status/row comparisons, all request signatures, no versionId
for the new case and unchanged compiled source hashes. Full C API1051 passes30
files/155 cases/745074 assertions with actual loaded-image and strict-signature
checks. Assertion counts contain workload-dependent loops, not a progress measure.

Current development shared SHA256:
`24ef809be18dc0b88c7045ff174dcb2c49ba12c12626dd99e04d5a2a3d613729`.
Source1053 preserves this component; it is not the final release candidate.

## Explicit diagnostic and request differences

255 of256 error fields match literally. The new error body matches, but Rust adds
a SQL line/caret while C++ returns that same body without query context. The raw
comparison is retained in `s3-body-only-audit-1052/literal-record-differences.json`;
it is not normalized away or claimed as exact error parity.

Both new-case traces have HEAD plus two GETs and no versionId query. Their ranges
show why the error arises at different stages:

| Engine | First GET | Second GET |
| --- | --- | --- |
| C++ |bytes4384064–4400447, a16KiB tail containing metadata|bytes4–4384063, data|
| Rust |bytes4400440–4400447, footer length|bytes4400068–4400439, metadata|

Rust detects replacement while binding metadata; C++ detects it on its later data
read. No prefetch/cache was added merely to reproduce C++ request scheduling or
remove this annotation. The total suite request counts remain82 Rust versus62
C++; all raw requests and independent signature checks are preserved. These are
request observations, not coupled speed/RSS or final performance acceptance.

## Remaining acceptance

The body-only pinning behavior is corrected. Its extra SQL diagnostic annotation,
301 Location handling, proxy absolute-form versus CONNECT behavior, credential
refresh and broader lifecycle/serializer cases remain explicit. Final common-
source signed release full SQL/shell/C API/ADBC, Python/Go/Swift and required-platform
matrices, persistence refresh and native coupled TPC-H/ClickBench time/RSS/outlier
acceptance remain open. Preserve the accepted uncached lower-memory policy and
conditional COPY exception; exact speed parity/outperformance remain unnecessary.

All receipts are on the external SSD under
`.context/artifacts/platform-matrix-20261029/s3-body-only-*` and source1053.
Prior encrypted matrix1041/1042 and CSV/source1036 receipts retain their own
immutable images; do not combine them into a final same-image release claim.
