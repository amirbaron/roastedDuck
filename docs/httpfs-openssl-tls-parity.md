# HTTPFS OpenSSL and trust-root checkpoint

2026-10-31. Development component evidence; final release acceptance remains open.

HTTPFS now uses a vendored OpenSSL TLS connector through the pinned UREQ3.5
transport interface. It replaces the Rustls backend rather than retaining two TLS
engines. Ring remains for S3 signing; rustls-pki-types remains a UREQ configuration
dependency, not a Rustls validation engine. The native-tls experiment is not in the
final dependency graph.

The connector searches the pinned C++ CA-bundle paths in their original order.
Explicit ca_cert_file paths load that file. On Apple, curl-mode validation can
supplement missing anchors with the native trust service, which validates the
chain and hostname. It does not override expiry, signature, hostname or key-usage
errors. Strict httplib mode with an explicit CA file uses that file without native
supplementation. This follows the distinction between C++ curl's native-CA option
and httplib's explicit verify-location loading. Upstream curl documents its
[native/custom CA behavior](https://curl.se/docs/sslcerts.html); the pinned local
sources are httpfs_curl_client.cpp and core third_party/httplib/httplib.hpp.

Verification settings retain their existing meanings and defaults. No certificate
validation is disabled to accept the previously failing CA-marked endpoint. The
OpenSSL chain/hostname checks now accept that case as the C++ reference does, while
rejecting wrong-host, expired and untrusted chains. Errors use typed verification
results to retain C++ diagnostics; underlying transport errors and the original
connection timeout budget are preserved through the TLS adapter.

TLS context reuse stays local to the existing per-handle agent. No object-body,
table or query cache or memory-budget increase was introduced. Existing
release_on_unpin and buffer-accounting policies are unchanged. Vendored native TLS
code changes build/packaging requirements; this checkpoint does not accept its
RSS or throughput on the final platform matrix.

## Verification

Evidence root: `.context/artifacts/platform-matrix-20261029/`, on this workspace's
dedicated external SSD. Aggregate TLS comparison: `openssl-tls-review-824`.

| Check | Result | Evidence |
| --- | --- | --- |
| TLS/HF acceptance, rejection and verification modes | 31 cases, 293 literal records match C++ | C++777, signed Rust817, review824 |
| Public system-root metadata and explicit-root policy | 12 literal records match | C++812, Rust825, review829 |
| Existing partition/delete controls | 30 SQL+11 SDK records match | partitions818, review828 |
| Existing upload controls | 35 functional records match; allocation sample remains 16515072 bytes each, zero before/after | writes819, review827 |
| Existing S3 read/list/limit controls | 30 cases, 276 records match | reads820, review826 |
| Existing HTTP/TLS/ETag controls | 16 cases, 121 records match | HTTP821, review826 |
| Existing extension controls | 59 cases, 246 records unchanged | integrated822 |
| Full public C API suite | 30 files, 155 cases, 792002 assertions pass | fullCAPI823 |
| Strict workspace/all-target Clippy | Pass | check813 |

The local matrix uses the original CA-marked certificate735 plus owned CA776 and
separate good, wrong-host and expired leaves. Its CONNECT receiver opens no
upstream sockets. Both engines accept the supported cases and reject invalid
certificates with verification enabled. Default httplib verification behavior and
explicit verification-disable options are checked separately against C++. HF
token and simulated CDN authorization controls remain included.

The separate public probe reads metadata from example.com with no custom CA and
verification enabled, in curl and strict httplib modes. Both succeed. With an
unrelated explicit CA, strict httplib rejects the connection on both engines.
This is one endpoint at one time, not exhaustive platform trust-policy coverage.
No roots were installed into a system trust store. An earlier content probe806
completed TLS but caught a real HEAD/GET ETag change; it remains preserved. The
metadata-only probe isolates trust validation without disabling ETag checks.

Only known package/TLS/output/owned-CA paths and actual loopback proxy endpoints
are mapped. SQL, statuses, values and diagnostics compare literally. Initial
comparison804 omitted the new validation-fixture path and showed seven setup-SQL
path differences; corrected comparison810 maps that known path without rewriting
errors or values. Request counts/range choices remain separately recorded. The
preexisting extra S3 listing per Rust prepared EXECUTE remains explicit.

Fresh build814 produced strictly signed shared815 and SDK816, authenticated by
whole-file pinned-certificate CMS before/after runtime. Shared native SHA-256:
`81768d3d5387b8f7f00ded04c701d926140f1ce6ebeeb2a2a19287c4c30101c3`.
The library retains 547 duckdb-prefixed exports and excludes private ADBC test hooks.
The C++ HTTPFS package remains authenticated data selecting the Rust provider;
its machine code is never executed by Rust.

The later same-image region/version/COPY checkpoint and preserved TLS regression
evidence are documented in [S3 region/version parity](httpfs-s3-region-version-parity.md).

## Preserved experiments and open gates

Fetch773 initially lacked rustc on PATH;774 succeeded. Native platform image780
still rejected the CA-marked endpoint's key usage in782, so it was not adopted and
verification was not weakened. Its source/config are preserved in the platform
attempt782 snapshot. OpenSSL check791 caught a callback reference-type mismatch;
style792 then caught documentation/import placement. Both were fixed before
the tested strict813/build814 source. No tests, skips or ratchets were weakened.

The former CA-marked acceptance discrepancy is closed for these Mac ARM64 controls.
Broader trust/distrust, malformed-CA, protocol, client-certificate and proxy cases
still need coverage, as do required Linux/iOS runtime builds of this connector.
These results do not imply full system-root or TLS-policy parity on all targets.

Remaining core work includes HTTPFS options, broader redirect/version edge cases,
credential refresh, directory/configuration/autoload/startup behavior, secret custom-serializer/
transaction compatibility, the isolated default HF token branch and real service
coverage. Final common-source signed artifacts still need full SQL/shell, C API/
ADBC and Python/Go/Swift matrices, cross-engine DB/WAL journeys and coupled speed/
RSS measurements with per-query outliers and documented acceptance. Mac Intel
and other clients remain outside scope. The complete goal remains active.
