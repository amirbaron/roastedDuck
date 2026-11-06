# HuggingFace filesystem and secrets checkpoint

2026-10-31. Historical HF checkpoint771; see [the newer OpenSSL checkpoint](httpfs-openssl-tls-parity.md). The complete release goal remains open.

HTTPFS now registers `HuggingFaceFileSystem` and the `huggingface` secret type.
`hf://datasets/...` and `hf://spaces/...` resolve files and revisions through the
pinned HTTPS endpoint, while wildcard paths traverse the tree API and its Link
pagination. `hf_max_per_page` is registered. Parsing, wildcard matching and tree
response handling follow HTTPFS commit
`4bc690dba4496c765777a0269d48fdbaff7cdc11`, including its minified type/path parser
and traversal behavior. The provider remains read-only.

Config secrets redact their token. Credential-chain precedence is HF_TOKEN,
HF_TOKEN_PATH, HF_HOME/token, then the optional default token file. File-based
tokens read the first line and discard CR bytes as C++ does. Empty HF_HOME resolves
`token` relative to the working directory. Missing explicit token paths produce
the reference diagnostic; the optional default-file failure is silent. HTTP
secrets still supply proxy/TLS/extra-header configuration. An HF secret overrides
the HTTP bearer token; extra Authorization headers retain C++ precedence.

HTTP handles now keep a logical filename separately from their transport URL.
This preserves an `hf://` filename in status diagnostics while HTTPS transport
errors continue to name the actual request. Object contents are streamed through
the existing positional reader, with no new object-body, table or query cache.
Existing memory budgets and `release_on_unpin` remain unchanged.

## Verification

Evidence root: `.context/artifacts/platform-matrix-20261029/`, on this workspace's
dedicated external SSD. Aggregate `hf-review-767/verification.json` compares actual
C++755 and signed Rust760 using the same public C API control source.

| Check | Result | Evidence |
| --- | --- | --- |
| HF files/tree/tokens/errors/redirects | 23 cases, 223 literal SQL records match C++ | review767 |
| Authorization | Scoped tokens and HTTP-header precedence match; CDN redirect strips Authorization on both engines | controls755/760 |
| Existing partition/delete controls | 30 SQL and 11 SDK records match C++ | partitions761, review770 |
| Existing upload controls | 35 functional records match; charged allocation sample remains 16515072 bytes each, zero before/after | writes762, review769 |
| Existing S3 read/list/limit controls | 30 cases, 276 literal records match | reads763, review768 |
| Existing HTTP/TLS/ETag controls | 16 cases, 121 literal records match | HTTP764, review768 |
| Existing extension controls | 59 cases, 246 records unchanged | integrated765 |
| Full public C API suite | 30 files, 155 cases, 773982 assertions pass | fullCAPI766 |
| Strict workspace/all-target Clippy | Pass | check756 |

The independent receiver accepts CONNECT only for `huggingface.co:443` and
`cdn.fixture.invalid:443`, terminates synthetic TLS locally and never opens an
upstream socket. Both engines explicitly trust the same test leaf certificate751.
No HuggingFace service or dataset download is involved. Cases exercise public and
scoped file reads, independent C++ Parquet input, HTTP/HF token precedence,
credential environment/file precedence, missing files, spaces/revisions, wildcard
and recursive traversal, pagination, malformed responses/Link headers, invalid
URLs, same-host redirects and a simulated CDN redirect without authorization.

Token controls use synthetic environment values and owned files. The empty-home
case temporarily changes only the test process's working directory to its owned
fixture directory. HOME is unchanged. The controls do not read the user's default
token file; that fallback still needs an isolated default-path control before
claiming complete credential-chain runtime coverage.

Only known package/TLS/output paths and actual loopback proxy endpoints are mapped
for comparison. SQL, statuses, errors and values otherwise compare literally.
Request counts/range choices are retained separately and are not required equal.
No test, skip or ratchet was weakened. The preexisting extra S3 listing request per
Rust prepared EXECUTE remains explicit in review768; no cache was added to remove it.

Shared image758 and SDK759 were freshly built757, normally codesigned, strictly
verified and whole-file CMS authenticated with the pinned certificate before/after
runtime. Shared native SHA-256:
`13c268dc921438a97252f5fc120a4195bfc0bc77cfbd785a7c16774dd4071fcf`.
The library retains 547 duckdb-prefixed exports and excludes private ADBC test hooks.
The C++ HTTPFS package remains authenticated data selecting Rust implementations;
its machine code is never executed by Rust. Generic C++ extension ABI support is
still absent.

## Preserved differences and remaining gates

The first C++ control736 crashed when the legacy accessor fetched the nested scope
column. The fixture now explicitly casts scope to VARCHAR before its legacy
scalar fetch; the rendered scope remains checked. Style lint738 and before739's
absent HF secret-type row are retained. Prototype753 matched the initial 22 cases/
213 records before the empty-HF_HOME control was added.

There is a known TLS acceptance distinction: C++ accepted CA-marked synthetic
certificate735, while Rustls rejected its use as a server leaf in744. Corrected
leaf certificate751 passes the workflow controls without disabling verification.
That repair does not prove equivalent acceptance of every certificate; full TLS
and system trust-root behavior remains an open gate.

Real service interoperability, broader URL/JSON/header/protocol cases and the
isolated default token-file branch remain unverified. Secret transaction and
persistent-file compatibility gaps documented in `secrets.rs` are unchanged.
Remaining HTTPFS work includes options, region redirects, refresh/version pinning,
full directory/configuration coverage and autoload/startup behavior, plus the
remaining core gaps.

Final common-source signed release verification still requires full SQL/shell,
C API/ADBC and Python/Go/Swift matrices, required Linux/iOS runtime targets,
cross-engine DB/WAL journeys and coupled speed/RSS measurements with individual
query outliers and documented acceptance. The allocation sample above is not peak
RSS or performance acceptance. Mac Intel and other language clients remain outside
scope. This checkpoint does not complete HTTPFS or the release goal.
