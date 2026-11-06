# Language-client completion

Current release verification (2026-11-03): source2078/signed macOS2088 and Linux ARM2080 supersede the retired source1959 artifacts after the concurrent-index correctness fix. Python3.10–3.14, Go and Swift are being verified afresh; prior client/SDK/simulator receipts below cannot accept the corrected artifacts. Required targets remain macOS arm64, iOS arm64 device/simulator and Linux arm64/x86_64. macOS Intel and other clients are outside scope. C API/ADBC remain required. Physical iOS hardware is unavailable and unverified, not waived. Internal-disk simulator execution is authorized; evidence stays on the external SSD. See [current verification](current-source-verification.md).

The owner narrowed the goal on 2026-10-25: required language clients are **Python,
Go and Swift only**. Preserve existing **C API and ADBC** functionality. R, Rust
client wrappers, Node.js, Java/JDBC, ODBC, WebAssembly, C#/.NET, Julia and other
language clients are outside this goal. Earlier expanded-scope notes are historical.
The engine target remains the frozen DuckDB 1.5.6 contract.

The [versioned client overview](https://duckdb.org/docs/1.5/clients/overview) and
[client repository inventory](https://duckdb.org/docs/1.5/dev/repositories) identify
upstream packages and tests. Pin matching revisions before implementation/testing;
a newer client or documentation update must not silently change the target version.

## Current selected-client verification (2026-11-03)

The candidate is frozen source1959/signed macOS arm64 core1961. The S3 worker
context correction changes two HTTPFS files; the five corrected private Python
binding files from1742 are included unchanged. Old core1637 and its client
receipts are historical, not acceptance evidence for the new candidate.

All five current macOS CPython3.10–3.14 normal ABI cells and independent1980
reviews pass. Matrix2024 exits0; summary2062 verifies31,135matching XML records
per engine,30,743passes and392retained nonpasses,36typed WAL journeys,216encrypted
Parquet cases,18installed ADBC environments,40typing checks and108coupled samples
across18separate workloads. Every workload remains in
[the binding report](python-fetch-conversion-parity.md), including slower Python-
backed rows. These observations do not establish TPC-H/ClickBench acceptance.

Both CP313fast profiles and CP314Arrow22 have two reference-only Pandas unraisable
attribute warning sites;2059/2060/2061 preserve actual raw logs and hashes.
Warning logs are not byte-identical. No warning suppression or private attribute
emulation was added.

Current C API1971 passes30harnesses/155cases/745,460assertions. ADBC1972 passes
64cases/3,040assertions per engine using a separate signed test bridge; shipping
core1961 retains547C API exports and zero private ADBC test hooks. Current Go1982 passes652cases per engine and1984 verifies four independent typed
checkpoint/WAL journeys. Full Swift1985 passes133cases per engine. Apple review2001 verifies the signed
three-slice SDK, relocated Mac consumers/four typed WAL journeys, full133case
simulator suite and typed simulator consumer/four additional WAL journeys.
Physical-device execution/provisioning is unverified. Both Linux core builds/sign2027 complete; ARM full SQL collection2010 is live
under dispatcher2069/session59928. Recoveries2063/2067 preserve and correct the
CWD-dependent verifier and missed preparation-pin failures before their payloads. PhaseB2047
covers the ten Linux Python ABI cells. Prepared checks are not passing evidence.
Physical-device runtime/provisioning and final coupled performance acceptance
remain open. See [current verification](current-source-verification.md) and
[required platforms](release-platform-matrix.md).

## Historical selected-client verification (source1634/1742; superseded)

Current matrix update: all five declared Mac CP310–314functional cells and
independent reviews now pass (index1831:31,135matching XML records per engine,
392retained nonpasses,36typed WAL journeys,108coupled client samples). Current
iOS ARM build1686is the sole heavy job in1819. Earlier phase updates below are
historical; all other platform/client/storage/focused-control and final native
macro acceptance gates remain open.

Update2026-11-03local: CP313now completes all15stages and independent review1822.
CP314strict build is live; other platforms and final macro acceptance remain open.
Reference-only Pandas/pytest warnings are retained in the binding report.

The selected scope remains Python, Go and Swift; C API and ADBC are preserved.
Common engine source1634/signed Mac core1637 is unchanged. Corrected Python
source1742 changes five private binding files, with explicit common-core
source/dependency equivalence1749.

Current macOS arm64 CPython3.10–3.14signed cells complete all required
functional stages and independent reviews1816/1822. CP310–313each match6,891raw
XML records per engine (6,804pass/87matching nonpasses). CP314matches3,571records
(3,527pass/44matching nonpasses) on its soleArrow22profile.
All420-record fetch-boundary and24-record HTTP profiles, buffer/Arrow producers,
typed checkpoint/WAL journeys, encryption,385exports/stubs, installed ADBC,
eight positive/negative typing checks and original non-fast suites pass.
Individual speed/RSS client rows remain in
[the binding report](python-fetch-conversion-parity.md). They do not replace
final TPC-H/ClickBench acceptance. CP313is independently reviewed; CP314strict build is running.

Current Mac C API1661passes155cases; ADBC1662passes64cases/3,040assertions per
engine with a separate test bridge and unchanged zero-hook shipping library.
Go1668passes652cases per engine and four typed WAL journeys1670. Swift1671passes
132upstream+one identity case per engine. Current SDK/relocated consumers/Swift
WAL, full simulator and physical-device proof remain distinct remaining gates.
Linux current artifacts/selected clients/API/ADBC/storage/WAL and final coupled
macro acceptance remain open. Follow [current verification](current-source-verification.md)
and the [platform matrix](release-platform-matrix.md); queued work is not a pass.

## Historical selected-client checkpoint (2026-11-02, superseded)

The signed source1623 development wheel1626 passes the complete macOS arm64
CPython3.10.16 fast suites on Arrow19 and21:3,241 and3,492 records per engine,
respectively. Every case ID, verdict and skip reason matches C++. The unchanged
HTTP/JSON/installer/S3 profile also matches all24records per engine on each profile,
including complete error JSON and response attributes. All six supplements pass,
including eight cross-engine checkpoint/WAL journeys with15typed columns and
48encrypted-Parquet cases across the profiles. Receipt1642 preserves the raw scope.

Installed-stub check1646 verifies six shared pinned stubs in both engines and the
seventh Rust forwarding stub. Runtime exports, strict positive/negative typing,
installed ADBC and non-fast checks1643–1647 are queued after the current core work;
these prepared checks have not passed yet. No complete Python ABI/platform cell or
whole release is accepted. Other Python ABIs/platforms, current Go/Swift consumers,
Apple/Linux runtime gates and coupled speed/memory acceptance remain required.
See [current-source verification](current-source-verification.md).

## Historical integrated verification frontier (2026-11-01)

Frozen core source1337/signed macOS arm64 release1344 has completed all4,870
original SQL files:4,826 passes,30 matching skips and14 documented divergences
(11 private C++ engine ABI demos and3 expected-OOM queries that succeed). No
crashes, timeouts or resource kills occurred. The exact unchanged C++934 image's
full baseline is retained with explicit source/runner lineage; see
[current-source verification](current-source-verification.md).

Fresh workspace build1361/runtime1410 completes148 harnesses:1,066 tests pass,
zero fail,7 remain explicitly ignored (review1412); two functional supplements
still require execution. Final C API1360 passes30 files/155 cases/747,104 assertions
on signed candidate1344. Full Mac shell review1421 preserves393 upstream passes/
2 skips, the768 differential's4 build-ID/1 native-frame differences, and21,460
byte cases (20,385 exact,676 native stack suffixes,399 unavailable Mac filename
fixtures). Every nonpass is retained;399 original fixtures must run on Linux.

Current native Mac ADBC1434 passes64 cases/3,040 assertions each. Storage1437
passes20 cross-engine DB/WAL journeys and40 tail-recovery checks. Go1438 passes
652 cases each, plus4 independent typed handoff journeys1425. Swift1443 passes
132 original+1 identity case per engine. Every image/source/signature guard is
retained. Runner-only scheduling, prerequisite and fixture errors remain recorded;
no engine or upstream test assertion changed.

First CP310 Linux x86_64 signed wheel1475 and4 fresh paired Arrow19/21
environments1375 pass build/sign/import/dependency checks. Both complete original
fast suites1376 match C++:6,733 records per engine, zero disposition/skip-reason
differences (review1480). Nine supplements1377 and explicit cell review remain;
zero of15 cells are accepted yet. Original build OOM is preserved, with build-only SSD-VM swap1472
and unchanged engine/release flags. All5 Mac dependency profiles are ready1430;
build1415/install1429/fullfast1433 are prepared but unexecuted.
Other Apple/Linux artifacts, Swift consumers/relocation/WAL/iOS runtime, Linux
client suites and coupled performance acceptance remain required. Historical
receipts below do not establish current common-core acceptance. Third-party
native crypto is allowed; the DuckDB engine remains Rust.

## Current correction frontier (2026-10-31)

Layout/signature checkpoint430 adds ten independent C layout compilations across
all five required target slices for the404-field v1.2.0 prefix and546-field full
table. Official core-signed inet bytes are accepted and a one-bit mutation rejected
with unsigned/community permissions disabled. Rust's data-only test explicitly
allows platform metadata mismatch to validate a Linux file on macOS; no downloaded
code ran. These do not close signed-C-ABI runtime or final platform/client gates.

Shared-library checkpoint420 adds actual public C API INSTALL/LOAD control402
and exact metadata-error408/initializer-error418 comparisons with C++ oracles.
It fixes validated string-option boolean handling, required .info mode fields and
initializer error categories. Workspace/all-target strict Clippy419 passes. These
remain development components; broad signed/version/lifetime/ABI/platform/client/
release/performance acceptance is still open.

Implementation checkpoint: the live bridge now passes native macOS SDK control360
for explicit unsigned loading (scalar/NULL/vector/repeated LOAD/catalog/reconnect,
two independent databases) and rejects the fixture with unsigned loading disabled.
Signed native unit355 passes independent OpenSSL signature/tampering/trust and
version checks; workspace/all-target strict Clippy361 passes. These are development
components. Initial local INSTALL now also passes signed native379 and exact C++
.info bytes373; latest workspace Clippy382 passes. Initial force/origin/gzip/permission-mask controls now pass385/391, matching file
modes0666 filtered by umask and directory0755 filtered by umask393; workspace
Clippy392 passes. Broader metadata/signature/lifetime controls and
shared-library/selected-client/platform refreshes,
complete metadata/signature/lifetime controls and final common-source release/
performance gates remain open. Source226 receipts remain before-image evidence.


Confirmed stable C-ABI loading gap341 now takes core priority: the pure-C plugin
passes the signed C++ oracle but Rust rejects LOAD. Existing API exports are present;
the loader/access bridge is missing. Further source226 ABI/platform builds were
unstarted and deferred321/343, preserving all inputs and prior component receipts.
After implementation, rebase final selected-client gates on the new common source.


Pandas3 defaults to dtype `str`. The signed old135 wheel rejects four original
before177 cases that C++ reads as VARCHAR, including empty and all-NULL strings.
Frozen source226 routes `str` through the same Arrow-buffer/Python-object binder
as `string*`. Signed235 CP313 Linux x86_64 now passes the unchanged original238
probe on Arrow19/21 and all58 expanded239 cases per engine:22+22 with Arrow and14
in a real environment without Arrow. Every raw C++/Rust record matches. No table
cache or whole-data materialization was added. All four complete baseline241 and
Pandas3 suites242 now match C++ exactly on Arrow19/21:3241/3492 records per engine,
including the actual Pandas3 new-string test. Nine supplements244 now pass,
including385 runtime exports/seven pinned stubs, strict typing, installed ADBC,
eight typed cross-engine checkpoint/WAL journeys,48 encrypted-Parquet cases,
producer/lifetime controls and coupled client measurements. Native ADBC247 now
passes all 64 cases/3,040 assertions per engine plus the public shipping consumer.
Go251 passes all 652 cases per engine and consumer252 passes four typed checkpoint/
WAL journeys. Swift254 passes 132 upstream cases plus one identity case per engine;
its consumers255 pass four typed checkpoint/WAL journeys. These are source226 Linux
x86_64/Rosetta components. Remaining ABI/target and final integrated gates stay open.

The fresh Apple ARM64 native slices and full iOS simulator Swift suite are queued
behind the current SQL/Python 3.10 gates. Native macOS paired C API296/297 and full
Go298/299, including four independent typed WAL journeys301/302, are prepared to
follow serially. The simulator device set and runtime use the authorized internal
disk; build outputs and verification evidence remain on the external SSD. Queued
components are unexecuted and do not change release acceptance.

Three interleaved fresh-process client medians under Linux x86_64/Rosetta are
Python-backed strings85.374ms/196.506MB Rust versus78.073ms/191.959MB C++ (1.094x
time/1.024x whole RSS,4.547MB excess); Arrow-backed18.509ms/207.032MB versus
114.499ms/248.300MB (.162x/.834x). Correctness and native identities pass. Preserve
all raw samples and this per-workload limit: these are not native macro acceptance
or evidence supporting a table-cache/memory increase. External aggregate253.

Reference224 passes22 Arrow and14 real-no-Arrow controls, covering values/types,
Unicode/NUL/NULLs, vector boundaries, Pandas3 copy-on-write snapshot semantics,
mutation, retained owners and closed connections. Minimal no-Arrow environments
pass dependency checks and actually lack Arrow. Raw old-Rust differences are preserved; both original and expanded controls now
pass on the new235 wheel as scoped above. This does not close full client acceptance.
The original218/223 control incorrectly expected mutation visibility; its failed
reference evidence is preserved, and224 asserts the observed C++ snapshot behavior.

Python3.10 hashing helpers now use a bounded SHA256 fallback when file_digest is
absent. Actual3.10.22 and host builtin/fallback file controls pass, with every
other caller AST unchanged. This verifies the helper, not the required15-cell
wheel/client matrix. Go, Swift, C API/ADBC, final target refreshes and native coupled
speed/RSS/outlier acceptance remain required. Older signed-client evidence follows.

## Current signed-candidate gates (2026-10-30)

The fresh macOS arm64 CPython3.13 wheel now matches both complete Arrow19/21
matrices, focused named-memory/ArrowString/object/producer controls and the full
package/typing/ADBC/non-fast/eight WAL/48 encryption refresh. Python-backed strings
measure1.180x time/0.992x RSS; Arrow-backed strings0.165x time/0.826x RSS. See
[macOS Python evidence](macos-python-client-parity.md). Python ABI versions beyond
3.13, other architectures and final macro acceptance remain open.

The newest Linux arm64 Python candidate fixes named-memory connections that
previously created persistent files. Byte-string pool keys also preserve raw
dot/separator names and independence from `chdir`. It matches11 frozen C++ controls
per profile, both complete fast matrices and all supplemental/package/ADBC/eight
typed WAL/48 encryption gates. Arrow-backed scans remain11.94ms/148.81MB versus
C++76.68ms/195.38MB; Python-backed strings remain1.217x time/1.005x RSS. See
[named-memory evidence](python-named-memory-parity.md). The corresponding macOS
arm64 refresh is verified above; other targets and final macro acceptance remain
open. Older checkpoints follow with their original artifact scopes.

The newer Linux arm64 ArrowString wheel matches both complete Arrow19/21 fast
matrices,23 focused cases per profile and the full installed-client supplement,
typing/ADBC, eight typed persistence journeys and48 encrypted-Parquet cases.
Its bind-time bounded Arrow conversion preserves repeat-query mutation behavior.
Arrow-backed string scanning measures10.90ms/149.23MB versus C++79.56ms/195.65MB;
Python-backed strings remain1.217x time at1.004x RSS. See
[ArrowString candidate evidence](python-arrow-string-scan-parity.md). Final integrated
macro performance and other-platform acceptance remain open. Older wheel checkpoints
below retain their original artifact identities and outliers.

Client integration is not final. The earlier Python object-buffer component passes
both complete Arrow19/21 fast matrices and its boundary/non-fast/typing/ADBC/WAL
gates; its1.21x/1.61x string-scan timing outliers remain explicit. That wheel predates
the separately verified Parquet core component and needs an integrated refresh.
See [Python object-buffer evidence](python-object-buffer-parity.md).

Linux arm64 now has optimized signed native artifacts and passes the full30-file
C API harness (155 cases/862,010 assertions), with no engine changes for Linux
harness portability. Linux arm64 Python subsequently matches both complete
Arrow19/21 fast matrices and non-fast/object-buffer/package/typing/persistence
controls. Its string-scan outliers remain explicit; see
[Linux Python evidence](linux-python-client-parity.md). Linux arm64 Swift now passes
all132 pinned upstream cases plus its separate identity test on both engines, with
matching IDs/verdicts and signed payloads verified before/after. Its relocated
Linux package and deployed independent consumers subsequently pass both normal
runs and all four typed checkpoint/nonempty-WAL handoffs; see
[Linux Swift evidence](linux-swift-client-parity.md).
Linux arm64 Go subsequently passes all652 pinned root-suite cases on both engines
with exact IDs/verdicts and a separately built module's four checkpoint/nonempty-WAL
handoffs. See [Linux Go evidence](linux-go-client-parity.md). Other target and final
integrated client acceptance remains open.
Linux arm64 native ADBC subsequently passes64 cases/3,040 assertions on each engine,
plus public-ABI transaction/Arrow/reopen checks on the feature-free shipping library.
See [Linux ADBC evidence](linux-adbc-parity.md) for the separate opt-in bridge scope.
See [Linux native evidence](linux-arm64-native-preflight.md) and the
[required platform matrix](release-platform-matrix.md). macOS x86_64,iOS device/
simulator and Linux x86_64 gates remain open.

The following integrated client checkpoint is an earlier macOS arm64 control;
it does not establish acceptance for the newer core or other platforms.

Resident source1 now passes both complete Python Arrow19/21 fast matrices with exact
C++ IDs/verdicts/skips, seven non-fast files (71 pass/eight shared skips), installed
ADBC, stubs/strict typing,385 runtime exports and eight typed checkpoint/WAL journeys.
Go passes652 pinned root cases plus a separate module/four cross-engine journeys.
Swift passes132 upstream+identity on both engines, relative/relocated/standalone
packages and four cross-engine journeys. Native ADBC passes64 cases/3040 assertions.
See [current signed client evidence](final-client-verification-20261029.md) for hashes,
platform boundaries and the remaining1.73x/1.92x Python string-scan outliers.
Final integrated performance, broader backend/core audits and platform verification
remain open. Platform scope is confirmed in the matrix linked above.

## Implementation history and earlier gates

A native PyO3 Python package is now implemented in `crates/python` and `python/`.
Its signed current development wheel matches 64 expression, 36 join/write, 49/51 native Pandas,
30 reader, 32 input, 42 foundation, 30 relation and 75/76 host C++ outcomes.
The DB-API cohort passes 101/101 on both engines; a broader host cohort
passes 37/37 on both engines; the object/analyzer cohort passes 79/81 (C++81/81).
The Arrow cohort passes 41/41 on both engines. Twelve discrepancy categories remain unaccepted: one Arrow setting change, two decimal cases,
string render mode, trailing UTF-8 aggregate/window identifiers, unstable numeric map
output, a mapped-view callback lifetime crash and JSON overflow/file-like pending-error failures and zero-batch Polars resource runaway and raced shared-context Polars corruption and legacy enum-pickle aborts and malformed temporary filesystem overwrite URIs in the reference. A separate relation
cohort now passes all 123 cases, matching C++. The 50
previously blocked cases now run; remaining failures are recorded.
This is a partial implementation, not Python completion. Native/Arrow UDFs pass all
seven frozen files (559 passed, one existing xfail),
with 26 focused outcomes matching C++. CSV read/write pass 98 cases with one existing
skip and 56 scoped outcomes; broad filesystem/platform coverage remains open.
Parquet read/write pass 57 primary cases; the broader cohort is 59/60 with a shared
C++ timestamp-unit failure. Fifty Parquet and 19 call-contract outcomes match with
documented nondeterminism normalized and raw text retained. Aggregate/window families pass 108 cases plus one existing skip; 79/81 focused
outcomes match, with two UTF-8 trim outcomes held as one unaccepted discrepancy.
Three type/conversion files pass 34/34 with 84 exact focused outcomes; two signed SDK
tests preserve streams, transactional type lookup and rollback. Native dataframe-map
callbacks pass 11 upstream cases and 82/84 scoped outcomes;
the two reference lifetime discrepancies remain recorded. Bounded numeric conversion
improves map timings without retaining table blocks. Profiling passes 2 upstream cases and 37 scoped comparisons, with real streaming
counters and four signed SDK stream/type tests. Frozen auxiliary Python modules and
licenses are packaged; both engines collect 3212 fast-suite cases without errors,
with nine shared dependency/version file skips still open. Collection does not prove
runtime parity. Native JSON input closes the remaining Spark failure: both engines
run 294 cases with 289 pass, four existing skips and one existing xfail. JSON has 105/109
isolated outcomes matching, with four unsafe reference error-state cases held unaccepted.
Eleven signed native JSON tests pass; three existing timing probes are ignored. Eighteen
row/NumPy/Pandas tuple comparisons match. Polars now passes 58 cases with two existing version
skips on both engines; replacement-scan/ownership cases pass 68/68, closing 26 Rust
failures. Focused Polars outcomes match 73/74; same-connection lazy self-join behavior
remains unresolved. The scoped 16-cohort run records 1668 pass/3 unchanged failures/
10 skip-or-xfail across 1681 cases. These component counts do not establish full Python
acceptance. The 1M eager Polars output speed/memory outlier remains documented. The additional explicit/default connection/result/value cohort
passes 126/126 on both engines after nine fixes; 56 runtime and 84 type focused outcomes
match. The latest 17-cohort run records 1794 pass/3 unchanged failures/10 skip-or-xfail
across 1807 cases, with current Clippy/workspace checks passing. Native dataframe append and closed-handle interrupt/progress checks
now pass 41 upstream cases on both engines and 52 focused comparisons. The current 18-cohort
run records 1835 pass/3 unchanged failures/10 skip-or-xfail across 1848 cases. A 15-column
Python append persistence journey passes cross-engine checkpoint and nonempty WAL
replay/read-write/readback in both directions. The 100-repeat Polars shared-context
probe adds an unaccepted reference corruption category; independent/sequential controls
pass. The runtime coverage receipt still lists 1372 collected cases in 131 files outside
current cohorts, plus missing-dependency/platform gates. The complete connection API file now passes 48/48 on both engines,
with 131 focused statement/metadata outcomes matching and eight signed SDK tests passing.
Native AST statements, discovery/load/install wrappers and default/path validation
are implemented; batch execution follows frozen preparation/result semantics.
Bidirectional enum-pickle protocols 2–5 pass; legacy C++ protocol 0/1 aborts remain
unaccepted. Current 19 cohorts record 1882 pass/3 unchanged failures/10 skip-or-xfail across
1895 cases; 1325 collected cases in 130 files and the dependency/platform matrices remain.
 All-types 205 and timedelta/offset 342 cases now pass on both engines;
dictionary/pushdown matches 155 pass/2 existing skips/2existing xfails after five fixes.
Producer projection/predicates and factory ownership are implemented; 20 focused
journeys include actual partition pruning. Polars pushdown/refeed/deprecation pass 72
cases after four warning/alias fixes. Current 23 cohorts record 2656 pass/3 unchanged
failures/14 skip-or-xfail across 2673 cases; 547 collected cases in 122 files and additional
environment/platform gates remain. 17 signed Arrow/SDK tests and compiler checks pass.
 Native registered-filesystem streaming and recursive VARIANT host
conversion close 40 more failures. The new cohort matches 65 pass/1 existing skip/3 existing
xfails; 85 focused VARIANT outcomes match and filesystem 15/16 retains a reference overwrite
URI defect as unaccepted. Current 24 cohorts record 2721 pass/3 unchanged failures/18 skip-or-
xfail across 2742 cases; 478 collected cases in 117 files and environment/platform matrices
remain. Known-size CSV buffer clamping removes a measured small-file memory outlier;
no table/result cache is added. General registered-backend database storage remains open.
 Native GEOMETRY WKB output and memoryview BLOB input close six more scalar failures;
the cohort is C++ 79/79, Rust 78/79, with the existing decimal-exponent discrepancy.
Focused scalar outcomes match 35/36; C++ ignores memoryview strides (thirteenth
unaccepted category). Current 25 nonoverlapping cohorts record 2793 pass/four held
failure assertions/18 skip-or-xfail across 2815 cases; 405 collected cases in 104 files
remain, plus dependency/platform matrices. Python Clippy and workspace checks pass;
no table cache is added. The remaining Pandas cohort closes 17 failures and matches
C++ 130 pass/one existing version skip; 31 focused boundary outcomes match. Current
26 cohorts record 2923 pass/four held failure assertions/19 skip-or-xfail across 2946
cases, leaving 274 collected cases in 73 files plus dependency/platform matrices.
Direct chunk string copies reduce measured scan time without a material memory
increase; the remaining 1.9×/2.0× dev-versus-release slowdown stays an individual open
outlier. The following Arrow/API cohorts close 13 Rust-only failures, including signal
cancellation. Arrow matches C++ 77 pass/two skips; API matches 86 pass/one shared local
timezone assertion/one xfail and, under UTC, 87 pass/one xfail on both engines. Current
28 cohorts record 3086 pass/five held assertions/22 skip-or-xfail across 3113 cases;
107 collected cases in 48 files remain plus dependency/platform matrices. Focused
statement/Explain checks match 175/175, reader boundaries 92/92 and cancellation/reuse
four/four; ten signed SDK tests and compiler checks pass. First-batch memory is bounded
for 1M/100M sources; latency differences remain separate and the cache policy is unchanged.
The remaining fast cohort matches C++ 100 pass/seven skips, closing five failures.
Whole-process `tests/fast` with pinned dependencies/TZ=UTC reports C++ 3183 pass and
Rust 3180 pass/three held Decimal assertions, with 38 shared skip/xfail records each
and no collection errors (3221 JUnit records, including nine file skips). Every one of
3212 original collected IDs is matched to execution records. Direct `_duckdb` import
first and 49 low-level helpers now work. Runtime exports are 362/385, with 23 missing;
typing-only declarations are separate, and native stub distribution remains open.
Seven non-fast files, missing dependency/version modules, broader signature/lifetime
audits and one raw Binder JSON key-order gap remain. No Python completion is claimed.
The 23 inventory availability gaps have since closed: all385 frozen runtime entries
are available. Constructors225/225, tokenizer298/298, explicit descriptor/lifetime13/13
and real CPU Torch51/51/TF51/51 comparisons match. Twelve signed SDK tests and compiler
checks pass; whole fast suite remains3180pass/threeheldDecimalassertions/38skip-xfail.
Callback metadata uses a scoped read-only native context; a guarded C++ reentry hang
is the fourteenth unaccepted reference discrepancy. Rejected eager-default memory
growth was removed; bounded before/after RSS and the ~6% insert-time difference are
retained. This availability/tensor checkpoint does not complete signatures, stubs,
the remaining environments/platforms/non-fast files or final acceptance.
R and the other language packages remain
unimplemented/unverified. The complete workspace/all-target check
includes the Python crate. See [Python implementation gates](python-bindings.md).

The subsequent non-fast/default-fixture run invokes all seven files: 71 passes and
eight skips per engine, including all30 H2O cases. Configured extension loading and
HTTPFS remain unverified (one reported pass is a fixture-unset no-op). The signed wheel
now distributes all six exact pinned native stubs and a relative-import forwarding
stub; installed strict typing clients and negative controls pass their expected checks.
This closes stub distribution, not every runtime signature or final Python acceptance.
Evidence is under `.context/artifacts/python-nonfast-20261025/`.

The wheel subsequently adds the frozen Python ADBC facade and Rust initializer in
its own native image. All 23 upstream ADBC tests now pass on both engines; installed
sequential file/transaction/UDF-write/reopen and one-image journeys match. All31 facade
declarations resolve; signatures/aliases remain partial in
`docs/python-adbc-inventory.json`. Complete fast reruns now have 3241 records:
C++ 3206 pass/35 skip-xfail; Rust 3203 pass/the same three held Decimal assertions/
35 skip-xfail, zero collection errors. All 3235 normal cases reconcile, including the 23 newly enabled
ADBC cases. Coupled timing/RSS stays bounded; Clippy/workspace checks pass. No full
Python, ADBC matrix, other-client or final release/performance completion is claimed.
Evidence: `.context/artifacts/python-adbc-package-20261025/`.

The PyArrow 21 matrix subsequently matches 886 passing Arrow cases, including 250
newly enabled run-end passes/two existing skips. Full fast reruns have 3492 records:
C++ 3456 pass/36 skip-xfail; Rust 3453 pass/the same three held Decimal assertions/
36 skip-xfail, zero collection errors. All 3487 normal cases reconcile. The Parquet
version-attribute guard remains a shared module skip; its unchanged test body passes
as a separately labeled adapted journey. PyArrow 19 controls and the signed ADBC wheel
are unchanged. Bounded prefix timing/RSS and the small latency difference remain in
`.context/artifacts/python-arrow21-20261025/`; broader client/final acceptance remains open.

The subsequent helper-boundary checkpoint fixes false-valued invalid connections
silently choosing the default, and preserves actual false-valued connections. Frozen
module diagnostics cover 68 helpers: 396 provided-argument comparisons, 1872 broader
boundary comparisons and 28 state/frame/exception checks match C++ (overlapping
cohorts, not one aggregate). The final signed wheel retains both complete fast-matrix
counts above and all 385 exports. Actual helper timings and RSS remain similar;
default database creation stays lazy. Broader argument-value conversions and complete
Python/client acceptance remain open. Evidence:
`.context/artifacts/python-root-contract-20261025/`.

The next Python checkpoint preserves byte aliases in connection sql/query/from_query
and relation set_alias, corrects from_query's keyword-only/empty alias default, handles
failed repr diagnostics/cached-after-close displays and rejects spoofed connection
types. Scoped matches: 2670 alias, 94 state, 432 connection, 1872 helper-boundary and
225 constructor outcomes (overlapping, not additive). A concurrent-render callback
count difference is explicitly retained; generated aliases use pattern/uniqueness
checks. Both full fast dependency matrices remain at the counts above, with no new
failures or collection errors. Typing, export and compiler gates pass; coupled RSS/
timings stay bounded. Evidence: `.context/artifacts/python-name-contract-20261025/`.
Other named-string boundaries and full client/final acceptance remain open.

Catalog-name support subsequently covers register/unregister/table/view, preserving
C++'s separate view fallback and raw-name semantics. The catalog cohort matches3993
outcomes, 18 state checks and two signed SDK regressions pass, and both full fast
matrices keep the same counts above. Strict fresh alias/state receipts preserve
hexadecimal decoder bytes after correcting an overly broad pointer normalization.
Typing/export/compiler gates pass. Metadata lookup adds a few microseconds per call
without higher RSS or a table cache; those costs are retained separately in
`.context/artifacts/python-catalog-names-20261025/`. Other client/conversion/platform
and final release/core/storage/WAL/performance acceptance remains open.

Table-function names subsequently match 842 frozen outcomes, including byte casters
and literal dotted names. Scan catalog names/signatures match C++; an owned-reader
journey and signed SDK stream/replacement regression pass. Arrow callback EOF/release
now flushes deferred Python decrefs, matching C++ input release without another query;
partial-reader shutdown checks pass. Both complete fast matrices keep their counts,
typing/export/compiler checks pass, and paired timing/RSS stays bounded. Evidence:
`.context/artifacts/python-function-names-20261025/`. Scalar UDF names and the broader
client/core/final acceptance gates remain open.

| Surface | Current evidence / remaining work |
|---|---|
| Python | Current post-Go-fix signed wheel refresh: both complete Arrow19/21fast matrices match CPP with zero failures;336Decimal,385runtime exports, exact stubs/strict typing and four current15column file/WAL journeys pass. Broader reference/signature/lifetime/nonfast/backend/platform acceptance remains open; see [current evidence](python-bindings.md) |
| R | Outside the current goal |
| C API | Current rebuilt library: all 546 duckdb.h exports; upstream 30-file harness passes; 8,735 native ABI outcomes match. ENUM invalid-value failure semantics remain open; duckdb_adbc_init now exists; scoped ADBC loading/interchange checks pass |
| CLI | Existing shell implementation and historical differential tests; close remaining engine/name gaps and repeat final shell verification |
| Native Rust | API exists; complete API review, byte-name integration and resource/error semantics |
| Rust client (`duckdb-rs`) | Outside the current goal |
| Go | Pinned v2.10506.0 dynamic root suite with Arrow passes652/652on Rust and CPP with identical case IDs/verdicts and selected Rust image verified. Five discovered core gaps fixed; portable installation, broader API/platform/performance and dependency-test gates remain open; see [Go evidence](go-bindings.md) |
| Node.js Neo | Outside the current goal |
| Java/JDBC | Outside the current goal |
| ODBC | Outside the current goal |
| WebAssembly | Outside the current goal |
| ADBC/Arrow | All frozen initializer callbacks exist; scoped native C++, real Python ADBC driver-manager and PyArrow comparisons pass. All 64 frozen upstream ADBC cases now pass on both engines with 3,040 assertions. Broader matrices, packaging and selected DuckDB client acceptance remain pending. See [ADBC inventory](adbc.md) |
| C#/.NET | Outside the current goal |
| Swift | Current macOSarm64Swift6.4: complete frozen132upstream cases plus separate identity test pass on CPPandRust with identical IDs; selected Rust DLL only is verified in process. Relative XCFramework package, relocated SwiftPM/standalone consumer and four bidirectional checkpoint/WAL client handoffs pass. Published distribution, broader API/platform/storage acceptance remain open; see [Swift evidence](swift-bindings.md) |
| Julia | Outside the current goal |

C++ private/internal ABI and C++-API extension binaries remain the original documented
engine limitation. This does not excuse Python, Go or Swift client APIs:
port adapters that depend on internals rather than loading the C++ DuckDB engine.
Deprecated/other community clients are not silently added to this confirmed scope.

## Evidence required for every selected client

Maintain a versioned API inventory with an implementation and evidence entry for each
public surface. Use upstream suites plus real client journeys that cover applicable:

- Installation/import/native loading and package artifacts, proving the Rust engine's
  library identity and absence of a C++ engine fallback.
- Connection and cursor/result lifetimes, parameters and prepared statements,
  transactions, error types/messages, cancellation and supported concurrency.
- Scalar and nested type conversions, NULLs, decimals, temporal values, strings/bytes,
  and statement/column metadata.
- Appending/ingestion, Arrow and host-native interchange, UDFs/callback lifetimes,
  relations/expressions and other documented APIs that the client exposes.
- Cross-engine persistence, checkpoint/reopen and shared-file/WAL behavior, including
  recovery after errors and cleanup on close.

For Python specifically, inventory DB-API, relational, expression, function/types and
interchange APIs, replacement scans and packaging. A minimal ctypes query wrapper or
SQL smoke test alone is not complete Python parity. Record version/platform-specific
limits and upstream test exceptions explicitly; do not mask missing features with mocks,
weakened tests or a C++ engine dependency.

Keep source and small coverage reports in the workspace; put package builds, downloaded
upstream sources/test snapshots, environments, caches and full logs in dedicated
`.context/artifacts/` subdirectories on the external SSD. Follow the owner's serial
workload rule. Complete the active core migration before rebuilding clients; prioritize
Python within the binding work, then address the remaining clients one at a time.
