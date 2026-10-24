# ADBC driver port

The target is DuckDB 1.5.6's frozen `src/common/adbc/adbc.cpp` and its vendored
ADBC 1.0/1.1 header, not the latest Arrow driver API. The Rust implementation is
**in progress**. `duckdb_adbc_init` is still unexported: the internal core below is
compiled and tested, but it is not an installable or driver-manager-ready driver.
Python ctypes oracle scripts are not Python language bindings.

## Implementation inventory

| Surface | Current state |
|---|---|
| Public C structures and all 56 callback slots | Rust declarations; 13 size/offset assertions match an independently compiled, signed frozen-header probe on arm64 macOS |
| Database lifecycle | New, configuration/path/URI options, Init, Release staged internally |
| Database option API | String Get, string/int/double Set staged; remaining typed callbacks pending |
| Connection lifecycle | New, Init, Release staged; unread streams materialize before disconnect |
| Connection options | String Get/Set, integer Get/Set, queued autocommit, current catalog/schema staged; remaining typed callbacks pending |
| Transactions | Commit/Rollback with continued manual transaction mode staged |
| Cancellation | Connection/statement interrupt callbacks staged; concurrent cancellation journeys and full status/detail verification pending |
| Statement lifecycle | New, Release, Prepare, SetSqlQuery staged; multiple SQL statements execute before preparing the last |
| Query execution | Streaming execution, rows affected, empty/comment query handling staged |
| Result schema / parameter schema | ExecuteSchema and GetParameterSchema staged |
| Arrow Bind / BindStream | Ownership transfer and per-batch parameter binding staged; C++'s one-row limit is retained |
| Arrow result stream | Schema, batch, last-error and release callbacks staged; reuse/release buffers unread batches without eager buffering at query start |
| Arrow single-batch stream | Independent schema copy including raw names, flags, nested fields, dictionaries and binary metadata staged |
| Statement option API / ingestion | Pending, including create/append/replace/create_append, temporary tables, catalog/schema selection and affected-row/error semantics |
| Connection metadata | GetInfo, GetObjects, GetTableTypes, GetTableSchema, GetStatistics and GetStatisticNames pending |
| Partitions / Substrait | Port exact frozen unsupported behavior and callback population; no generic placeholder implementation accepted |
| ADBC 1.1 error API | Error details, ErrorFromArrayStream and detail ownership pending |
| Initializer | Pending callback wiring and version-prefix bounds verification; do not write a full 1.1 struct into a 1.0 allocation |
| Packaging / driver manager | Pending actual upstream driver-manager and selected-client journeys |

The module temporarily allows dead code because staged callbacks are intentionally
not published as a partial driver. Remove that allowance as initializer integration
makes the callbacks reachable. Production code in this directory is Rust; C++ in
`crates/capi/tests/adbc_core.cpp` is a reference verification probe only.

## Verified core checkpoint, 2026-10-24

Seven signed development-profile C API library tests pass, zero ignored; six are ADBC
regressions and one is the existing Arrow test. They exercise real engine execution:

- A partially consumed 5,000-row result survives another query; a second result
  remains readable after statement, connection and database release.
- Native invalid-UTF8 column metadata, multi-statement SQL, commit/rollback, and
  positional parameter schema match the frozen behavior in the tested scenarios.
- Schema inspection without execution and invalid SQL errors are checked. Ordinary
  errors containing `interrupt` remain INVALID_ARGUMENT; only the exact frozen
  `Interrupted!` message is classified as cancellation.
- Single-batch parameter binding produces 42, clears transferred input releases,
  preserves independently owned schema output, and rejects a two-row batch with
  C++'s NOT_IMPLEMENTED message. Bound-stream release ownership is transferred.
- Nested/dictionary schema copies preserve flags, native names and embedded-NUL
  metadata after the input stream is destroyed.
- A legacy 1.0 error-prefix canary remains intact; option reads preserve short
  buffers and report the required NUL-inclusive length.

The maintained signed C++ public-ABI probe passes three corresponding journey families:
stream lifetime, manual transactions/parameters, and bound parameters/row limits/error
classification. It is linked only to the frozen C++ reference. Rust's tests call the
internal Rust callbacks directly; there is **no Rust driver-manager acceptance yet**.

C API all-target Clippy with warnings denied and complete workspace/all-target checking
pass. These are core integration checks, not full upstream ADBC conformance. Full ADBC
upstream tests, deferred stream errors/details, ingestion, metadata, typed options,
versioned initializer canaries, concurrent cancellation and all selected language
packages remain open.

Evidence lives on the external SSD under `.context/artifacts/identifier-bytes-20261023/`:
`adbc-core-tests-6-summary.json`, `adbc-core-tests-6-0-duckdb.log`,
`adbc-core-test-build-6.jsonl`, `adbc-core-cpp.log`, `adbc-layout.txt`,
`adbc-core-clippy-5.jsonl`, `adbc-core-workspace-check-1.jsonl`, and
`adbc-core-checkpoint.json`. The checkpoint records exact source and evidence hashes.
The accepted uncached speed-memory policy is unchanged; no table-cache retention was
introduced. No new final performance claim follows from these functional tests.
