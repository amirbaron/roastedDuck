# Language-client completion

The owner expanded the goal on 2026-10-23 and confirmed this scope: **all primary
clients plus C#/.NET, Swift and Julia**, with Python first. Python/R are no longer
excluded from completion. The engine target remains the frozen DuckDB 1.5.6 contract.

The [versioned client overview](https://duckdb.org/docs/1.5/clients/overview) and
[client repository inventory](https://duckdb.org/docs/1.5/dev/repositories) identify
upstream packages and tests. Pin matching revisions before implementation/testing;
a newer client or documentation update must not silently change the target version.

## Current inventory and gates

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
| Python | Signed wheel6: both complete Arrow19/21fast matrices match CPP with zero failures;336Decimal,385runtime exports, exact stubs/strict typing and four scoped15column file/WAL journeys pass. Other reference/signature/lifetime/nonfast/environment/platform acceptance remains open; see [current evidence](python-decimal-parity.md) |
| R | New bindings/package required; inventory and implement the public API and run upstream tests |
| C API | Current rebuilt library: all 546 duckdb.h exports; upstream 30-file harness passes; 8,735 native ABI outcomes match. ENUM invalid-value failure semantics remain open; duckdb_adbc_init now exists; scoped ADBC loading/interchange checks pass |
| CLI | Existing shell implementation and historical differential tests; close remaining engine/name gaps and repeat final shell verification |
| Native Rust | API exists; complete API review, byte-name integration and resource/error semantics |
| Rust client (`duckdb-rs`) | Pin client revision; build/link against this library and pass its upstream API/tests |
| Go | Audit native dependency/packaging; run the matching client API/tests against the Rust engine |
| Node.js Neo | Complete native adapter/packaging as needed; pass its API and upstream tests |
| Java/JDBC | Audit native adapter and package; port dependencies on engine internals and pass JDBC/client tests |
| ODBC | Complete driver/adapter, packaging and upstream conformance/client tests |
| WebAssembly | Build the Rust engine for the client runtime; verify browser/worker and supported host/package journeys, without a bundled C++ engine |
| ADBC/Arrow | All frozen initializer callbacks exist; scoped native C++, real Python ADBC driver-manager and PyArrow comparisons pass. All 64 frozen upstream ADBC cases now pass on both engines with 3,040 assertions. Broader matrices, packaging and selected DuckDB client acceptance remain pending. See [ADBC inventory](adbc.md) |
| C#/.NET | Pin the client; verify its complete API/tests and native-library loading/packaging |
| Swift | Frozen1.5.6bundled wrapper inventoried:28source files/9test files/132test methods; all105called C API functions exported. Rust-linked package adaptation/build/tests and loading/ownership/type/storage acceptance remain open; see [Swift scope](swift-bindings.md) |
| Julia | Pin the client; verify its complete API/tests, native loading and ownership behavior |

C++ private/internal ABI and C++-API extension binaries remain the original documented
engine limitation. This does not excuse Python/R/Java or other selected client APIs:
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
