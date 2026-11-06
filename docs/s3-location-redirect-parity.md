# S3 Location redirects and binary upload responses

The current Mac ARM64 development candidate1088/1089 follows signed-request
Location redirects before bucket-region handling. It passes36 independent wire
cases/405 status-row records,80 method/status/origin/client policy cases per engine,
redirect-limit comparisons,31 TLS cases/293records and155 C API cases. Four signed
unit tests cover URI/origin and existing forward-transport framing behavior.
Final release/client/platform/full SQL and coupled performance acceptance remain open.

## Derived C++ behavior

The stronger1071 control distinguishes a redirected HEAD from metadata fallback.
C++ sends HEAD301→HEAD200; prior Rust1063 sent HEAD301→rangedGET206 and returned the
same data without following Location. The initial query-only1070 control was too
weak and is preserved separately, not counted as redirect proof.

Authenticated C++1072 records80 combinations of HEAD/GET/PUT/POST,301/302/303/307/
308,same/cross origin and Curl/httplib. Its receivers accept destination headers
without checking their SigV4 validity to observe forwarding transformations; this
is policy evidence, not signature-validation proof. Independent signed-wire
controls remain in s3_version_control.py.

Curl changes POST to GET on301/302/303, while retaining other tested methods.
Httplib changes non-GET/HEAD to GET on303 and clears its headers/body;301/302/307/
308 preserve tested methods. C++1073 permits30 redirects for Curl and20 for httplib,
then fails on the31st/21st request. Rust1093 matches those counts and error reasons.
The relevant primary source is the frozen1.5.6 httplib redirect implementation;
the native oracle is source-built934 plus official HTTPFS590, not an official
DuckDB engine distribution. Its dummy build ID remains in the provenance.

## Implementation and credential boundary

HttpFile now handles Location before its existing bucket-region retry. Redirect
state is local to the request; no object/query cache is added. It retains the
original signed headers within an origin, follows the observed method rules and
bounds, resolves HTTP(S) relative Locations, and restarts original request state
when bucket-region handling legitimately re-signs the request.

On an origin change, the port clears all scoped original headers, including
Authorization and session tokens. The C++ probe drops Authorization but forwards
the synthetic x-amz-security-token in38 cross-origin cases. Rust deliberately does
not reproduce that credential disclosure. Both engines' redirected methods,
Authorization presence and status/result rows match all80 cases; the38 synthetic
token-header differences are explicitly retained in audit1097. No real credential
is used or sent outside loopback. This is an intentional header-policy exception,
not a claim of exact C++ header equivalence.

A follow-on failure appeared when httplib303 changed PUT to GET and the successful
response contained Parquet bytes. C++ accepted that success response while Rust
failed strict UTF-8 decoding. A direct binary PUT-success control1085 reproduced
it independently. Successful PUT now streams/discards its response body and uses
status/ETag; XML-inspected responses decode malformed bytes without an unrelated
UTF-8 I/O failure. It does not retain binary success bodies or change upload batch
memory accounting. This matches the observed C++ behavior, including method rules
that can report COPY success after a303 GET; the probe does not claim an upload
occurred at that redirected target.

## Verification and preserved evidence

- 1071: C++positive/Rustnegative redirectedHEAD wire sequence.
- 1072/1073: C++80-case policy and redirect-limit observations.
- 1074: one style lint;1075 check and fresh1076 build pass; signed1077/runtime1078
  passes four URI/origin/forward-framing tests.
- 1079/1080: initial strict/fresh candidate1081/1082 passes29 wire controls but
  policy1084 exposes binary success-response failures.
- 1085: independent C++positive/Rustnegative binary PUT-success response.
- 1086/1087: final strict workspace/all-target check and fresh native build succeed
  before signed1088/1089 execute.
- 1090/1091: both engines pass36 signed wire cases,405 matching status/row records.
  Seven Location forms check HEAD301→HEAD200; default Curl also checks path-relative
  form, and httplib checks absolute/root/scheme-relative forms.
- 1092/1096: final80-case replay and repository reusable policy script pass on both
  engines with explicit Rust cross-origin token rejection.
- 1093: matching31/21 request limits;1094:31 TLS cases/293records pass;1095:30 public
  C API files/155 cases/724410assertions pass, selected image/signatures verified.
- 1097: final component audit;1098 existing writes pass35 functional checks plus
  one allocation sample (36 records):16515072 EXTENSION bytes,zero before/after.
  This is not RSS. Source1099 archives the current code and controls.

All evidence remains external under `.context/artifacts/platform-matrix-20261029/`.
Repository control s3_redirect_control.py accepts library/package/independent
Parquet/output arguments and --engine cpp|rust; callers authenticate before/after.
The targets never open upstream sockets. All keys/tokens/proxy credentials are
synthetic. Previous failures, prototypes and frozen artifacts remain unchanged.

Current signed development shared SHA256:
`d45d41bac9055d309a5ca368d02cc62ef157811833eac4126c51f0c914856d54`.
No dependency or cache policy changed. The accepted lower-memory baseline and
conditional COPY memory-throttling exception remain intact.

## Explicit remaining differences and gates

404 of405 error fields match exactly; the prior body-only ETag case retains the
extra Rust SQL line/caret. Request counts remain140 Rust versus102 C++. These
include different metadata-range scheduling; no prefetch/cache was added to force
C++ request patterns. They are not coupled speed/RSS acceptance measurements.
The origin-change token policy above is deliberately stricter. These results do
not establish all malformed URI/header combinations or every deployed HTTP client.

Credential refresh, secret lifecycle/custom serialization and broader proxy/
configuration cases still need verification. Then freeze final source and verify
signed release SQL/shell/C API/ADBC, Python/Go/Swift and required platform matrices,
repeat persistence on final artifacts and run native coupled TPC-H/ClickBench with
all query outliers. Mac Intel and extra clients remain excluded; native crypto is
allowed while the DuckDB engine remains Rust. Goal active/incomplete.
