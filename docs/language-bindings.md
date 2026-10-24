# Language-client completion

The owner expanded the goal on 2026-10-23 and confirmed this scope: **all primary
clients plus C#/.NET, Swift and Julia**, with Python first. Python/R are no longer
excluded from completion. The engine target remains the frozen DuckDB 1.5.6 contract.

The [versioned client overview](https://duckdb.org/docs/1.5/clients/overview) and
[client repository inventory](https://duckdb.org/docs/1.5/dev/repositories) identify
upstream packages and tests. Pin matching revisions before implementation/testing;
a newer client or documentation update must not silently change the target version.

## Current inventory and gates

No standalone Python, R or other language-package implementation was found in the
tracked repository inventory. The existing Rust engine/C ABI is not proof that upstream
packages load or pass their APIs. The native-byte migration now compiles across the complete workspace/all-target check
(2026-10-24). Current scoped C API evidence below does not prove final client acceptance.

| Surface | Current evidence / remaining work |
|---|---|
| Python | New bindings/package required; inventory and implement the public API, then run upstream tests and installable-package journeys |
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
