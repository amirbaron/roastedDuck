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
passes 37/37 on both engines; the object/analyzer cohort passes 77/81 (C++81/81).
The Arrow cohort passes 41/41 on both engines. Seven discrepancy categories remain unaccepted: one Arrow setting change, two decimal cases,
string render mode, trailing UTF-8 aggregate/window identifiers, unstable numeric map
output and a mapped-view callback lifetime crash in the reference. A separate relation
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
Native dataframe-map callbacks pass 11 upstream cases and 82/84 scoped outcomes;
the two reference lifetime discrepancies remain recorded. Bounded numeric conversion
improves map timings without retaining table blocks. R and the other language
packages remain unimplemented/unverified. The complete workspace/all-target check
includes the Python crate. See [Python implementation gates](python-bindings.md).

| Surface | Current evidence / remaining work |
|---|---|
| Python | Native arrays, Arrow, joins/writes and expression trees; 101/101 DB-API, 37/37 host, 41/41 Arrow, 77/81 object and 123/123 relation/expression cases pass. 559 UDF cases pass plus one existing xfail; complete remaining relation/input/expression and UDF overload APIs, platform/package gates and seven unaccepted discrepancy categories |
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
| Swift | Pin the client; verify its complete API/tests, native loading and ownership behavior |
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
