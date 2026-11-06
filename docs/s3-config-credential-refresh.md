# S3 config-provider credential refresh

Current signed Mac ARM64 development1117/1118 supports config-provider REFRESH
and REFRESH_INFO, with one-shot authentication renewal. Thirteen independent
SigV4 receiver cases pass on both engines, including HEAD/GET/PUT/multipart/list,
endpoint replacement and no-change/disabled/absent cases. This is a scoped current
component, not complete provider/lifecycle or final release acceptance.

## Derived behavior and implementation

Authenticated C++934 plus officialHTTPFS590 accepts refresh_info MAP(VARCHAR,VARCHAR)
and stores a typed STRUCT of replacement creation options. After401/403 or
400 ExpiredToken it recreates the selected secret with its provider/name/scope/
storage/persistence, reloads auth/HTTP material and retries once if material changed.
Other400,disabled refresh,no metadata and unchanged material do not cause a retry.
The local primary source is pinnedHTTPFS4bc690d create_secret_functions.cpp and
s3fs.cpp; direct public-CAPI reference1100 confirms built-in config applicability,
without a custom C++ provider. This is not an AWS extension/provider claim.

The Rust parser now builds typed refresh metadata from cast options and rejects
simultaneous refresh/refresh_info. The engine callback recreates matching config
secrets in the active transaction, serializing provider refresh calls per database.
The transport checks reloaded material before invoking refresh, retries auth at
most once per request, and keeps replacement signing/agent/HTTP parameters for
later operations on the handle. Learned bucket-region/version state and operation
query/content metadata remain; listing/bulk paths retain bucket-level addressing.
Replacement transport owns a separate state to avoid an Arc cycle. No object or
query data cache is added, and upload memory accounting is unchanged.

401/403 are authentication failures;400 requires ExpiredToken in its XML body.
Inspected error bytes are restored for subsequent error handling. A refreshed
endpoint/proxy/header/TLS material set creates the appropriate transport; unchanged
material does not cause repeated network retries. Refresh errors preserve their
kind and identify the secret to recreate/remove. Persistent file/catalog semantics
reuse the existing secret-manager implementation; expanded persistent-refresh
cross-engine journeys still remain to verify.

## Privacy exception

C++ default redacted secret_string exposes replacement credentials inside the
refresh_info struct in reference1100. All values there are synthetic. Rust redacts
the entire nested refresh_info by default; explicit authorized unredacted listing
still returns the typed values. This is an intentional privacy difference, not
literal default secret-string equivalence. No real credentials are used by these
controls or copied into reports.

## Evidence

-1100: direct C++ config-provider discovery for401/403/ExpiredToken,other400,
 disabled and missing info.
-1101–1104: initial compiler/style failures preserved;1105 checks pass.
-1108: initial unit setup used the default store; assertions exposed unrelated
 entries. Tests now set a private directory before open. No persistent user secret
 was changed.1111 exposed startup-only allow_unredacted_secrets; configuration was
 moved to startup. Signed1113/runtime1114 passes all three isolated tests.
-1120: final strict workspace/all-target checks pass after checked path conversion;
 fresh1116 succeeds before shared1117/SQL1118 are signed and authenticated.
-1119: six independent signature-checked refresh cases pass on both engines.
-1121: expanded receiver initially allowed stale GET after failing only HEAD;
 that fixture flaw is retained. Corrected1122 passes12 cases;1123 adds multipart
 and passes13 on both engines, checking actual upload bytes and all signatures.
-1124: current regressions pass36 signed wire cases,35 write functional records
 plus allocation sample,31 TLS cases/293records and40 secret-transaction cases on
 both engines. All644 transaction status/row/error observations match after only
 endpoint/path substitutions. Current C API1128 passes30 files/155 cases/705400 assertions with selected-image/strict-signature checks.
-1129: component audit; source1130 preserves code/controls and immutable identities.

Upload allocation remains16515072EXTENSION bytes,zero before/after; it is not RSS.
The accepted uncached lower-memory policy and conditional COPY tradeoff are unchanged.
Current signed native SHA256: `db8503878c3896857299d428810420205dc3f2c96479c1dcc1562725426082c6`.
The C++ engine comparator is source-built1.5.6 with recorded dummy build ID, not
an official engine distribution. The HTTPFS package is authenticated official590.
All evidence is external under `.context/artifacts/platform-matrix-20261029/`.

## Remaining acceptance

This does not prove every refreshable provider, persistent-refresh migration,
concurrent/cancellation/failed-commit lifecycle or custom serializer. Custom C++
provider APIs must be distinguished from the port's intentional C++ binary-ABI
boundary; review applicability before adding work. Existing pure config creation,
transaction and storage formats remain supported. Current images still need full
same-source signed release SQL/shell/C API/ADBC, Python/Go/Swift/platform matrices,
persistence refresh and coupled native TPC-H/ClickBench time/RSS/outlier acceptance.
The cross-origin token policy and extra ETag query-context diagnostic remain explicit.
Goal active/incomplete; no Mac Intel or additional clients required.
