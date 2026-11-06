# Aggregate callback and bind-data equality

Verified component checkpoint2026-10-29. The correction passes signed debug/release
regressions, current shipping-image integration and affected SQL/C API suites.
The whole port goal remains active; final release and performance acceptance remain.
Required clients remain Python, Go and Swift only; preserve C API/ADBC functionality.

## Confirmed public discrepancy

The retained signed FSST image computes the same values and column names as C++,
but duplicates aggregate states for median/quantile, variance/stddev and Kahan aliases,
including grouped/window, FILTER, DISTINCT and ordered cases. Parameterized aggregates
can also fail to share identical calls because separate bind-data instances default
to unequal. Independent frozen C++ controls retain complete physical and serialized
plans, raw legacy API records, explicit SQL-cast values and column names.

DuckDB1.5.6 AggregateFunction equality compares six callbacks: state size,
initialization, update, combine, finalization and window. It ignores function names
and the simple-update callback. Expression equality separately checks logical return
types, children, filters, DISTINCT/order modifiers and bind data. Rust previously
compared aggregate names/signatures and omitted the base logical-return-type check.

C API functions use shared engine wrappers for those six callbacks. Their bind data
compares extra_info plus user update/combine/finalize callbacks. Rust already implements
that C API bind-data comparison; comparing operation instances or substituting raw user
callbacks for wrapper identity would change the C++ contract. The C++ wrappers also
supply no simple-update callback, selecting hash aggregation for ungrouped C API calls.
The earlier Rust path always selected the simple physical operator.

## Working correction

AggregateOps now supplies the identities of its callback family, using concrete Rust
operation types independently of function names, allocation addresses and linker code
folding. Exported-state operations preserve forwarded identities and replace the
finalizer/window slots. C API wrappers retain one shared family and their existing
bind-data checks. A simple-update capability selects the C++ physical aggregation path;
the Rust fallback update method remains available to internal callers.

Bound expression equality now checks logical return types like C++. Eight aggregate
bind-data types implement the frozen comparisons: decimal average scales, string
separators, bitstring bounds, arg_min/max NULL policy, exact quantile values/order/
direction, approximate quantiles, reservoir quantiles/sample sizes, and sorted
aggregate inner functions/bind data/order descriptors. Exact quantile probability
Values compare numerically, including equivalent DECIMAL scales and DOUBLE literals.
Different parameters, filters, DISTINCT modes, callback/extra-data and orderings must
remain separate. No table cache was added; sorted descriptors serve bind-data equality.

## Verified scope so far

Source5 preserves961 overlay entries/960 build inputs with before/after guards.
Workspace/all-target Clippy passes with warnings denied. Signed debug and optimized
SDK regressions each match36 complete frozen C++ physical plans, values and raw column
names, plus eight native C++ logical-return-type controls. A broader affected-crate
run passes142 tests across22 signed executables; one existing optional full cast-
oracle CLI tool remains ignored, with its checked-in subset passing.

The shipping build exited0. Frozen CLI/library/SQL runner signatures pass strictly;
the library retains547 C API exports and zero ADBC test hooks. The50-case public
native matrix matches complete physical and serialized plans, raw API records,
explicit SQL-cast values and result column names. It includes empty inputs, moving
window frames, ordered single-row C API aliases, and callback/extra-data negatives.
The shared export-state serialization exception matches literally.

The affected SQL run covers324 aggregate/window/subquery/serialization files:
323pass and one exact C++ longdouble prerequisite skip, with zero failures,
crashes, timeouts, verdict regressions or ratchet changes. All30 C API suite files,
155cases and747436assertions pass; each loader selects the immutable current library.
Source/fixture/image hashes and strict signatures are verified before and after.

Three additional multi-row ordered C API controls expose a C++ reference defect.
SortedAggregateFunction passes a CONSTANT_VECTOR when no simple-update callback
exists; CAPIAggregateUpdate reads it through FlatVector::GetDataUnsafe without
flattening. In this frozen build,20-row calls provide a valid first state and a
NULL second state. A checked callback rejects the invalid array through the public
error API; no invalid state is dereferenced. Rust supplies valid repeated pointers
and independently returns190/190,190/380 and190/190 for the three controls.
All three physical and serialized plans still match C++ exactly. These reference
faults are retained separately from50 paired result controls. Single-row ordered
C API controls exercise the defined reference path and match literally. See
`conformance/divergences.md` for the narrow reference-comparability exception.

Current library SHA256:
`e9b447335ee2c5fba4bcb399092d74676e85284a1acdfa769ecd0f2d62c2372d`.
CLI: `6798e2fec6736382a74900dfb43663a92e83b8be145e8cb82d1795cf1e5409e8`.
SQL runner: `846188c762c3801ae79a1f819bbff6ebe872f840473c1d524d07201e3e3dd301`.

Final whole-corpus, shell, selected-client, ADBC, persistence/WAL and coupled
TPC-H/ClickBench speed/RSS/outlier acceptance remain required after core work is final.
The accepted lower-memory policy and explicit COPY tradeoff remain unchanged.

## Evidence and retained failed attempts

External task: `.context/artifacts/aggregate-equality-20261029/`. Current receipts:
`source-5/manifest.json`, `clippy-5-terminal.json`, `sdk-debug-5/verification.json`,
`sdk-release-1/verification.json`, `affected-debug-1/verification.json`.
`before-5/` calibrates47 signed C++/old-Rust controls; `before-7/` calibrates50
including defined single-row ordered C API calls. Current native receipts:
`native-images-1.json`, `native-matrix-1/verification.json`,
`affected-sql-1/verification.json`, `capi-suite-1/verification.json`,
`ordered-state-1/verification.json`. The native return-type oracle/eight-case output,
initial uncontrolled callback exceptions in `before-6/`, and strict checked-state
reference fault traces remain separately archived.

The initial summary omitted GROUP_BY nodes; full raw plans were retained and the
summary filter was corrected. The first SDK fixture compared a JSON-typed Value
with a VARCHAR and revealed still-unimplemented quantile bind-data equality.
Subsequent name-format/closure checks failed in the new test harness and were fixed.
The raw legacy C API text getter yields no LIST value on both engines; the stronger
fixture obtains defined nested values through explicit SQL casts. Original raw API
records and all failed snapshots/logs remain separate. Frozen C++ plan expectations,
SQL ratchets and performance policy were not relaxed.
