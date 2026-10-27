# ADBC driver port

The target is DuckDB 1.5.6's frozen `src/common/adbc/adbc.cpp` and its vendored
ADBC 1.0/1.1 header, not the latest Arrow driver API. The Rust implementation is
**in progress**. The initializer is now exported and all 54 callbacks assigned by the
frozen C++ initializer are wired. Real driver-manager loading and scoped client journeys
pass. The complete frozen 64-case upstream suite now passes on both engines. Broader
coverage, packaging and the separate DuckDB Python API/package goal remain open. The oracle scripts and Apache ADBC client are not that Python package.

## Implementation inventory

| Surface | Current state |
|---|---|
| Public C structures and all 56 callback slots | Rust declarations; 13 size/offset assertions match an independently compiled, signed frozen-header probe on arm64 macOS |
| Database lifecycle | New, configuration/path/URI options, Init, Release staged internally |
| Database option API | All typed callbacks staged; finite/NaN/infinity/negative-zero string conversion and representative error/buffer semantics verified |
| Connection lifecycle | New, Init, Release staged; unread streams materialize before disconnect |
| Connection options | All typed callbacks staged, including frozen unsupported operations; queued autocommit and current catalog/schema staged. Catalog/schema reads now use the frozen query/Value boundary; broader native option/error coverage remains open |
| Transactions | Commit/Rollback with continued manual transaction mode staged |
| Cancellation | Signed cross-thread connection/statement cancellation and subsequent query recovery match C++; stream status/details are checked |
| Statement lifecycle | New, Release, Prepare, SetSqlQuery staged; multiple SQL statements execute before preparing the last |
| Query execution | Streaming execution, rows affected, empty/comment query handling staged |
| Result schema / parameter schema | ExecuteSchema and GetParameterSchema staged |
| Arrow Bind / BindStream | Ownership transfer and per-batch parameter binding staged; C++'s one-row limit is retained |
| Arrow result stream | Schema, batch, last-error and release callbacks staged; reuse/release buffers unread batches without eager buffering at query start |
| Arrow single-batch stream | Independent schema copy including raw names, flags, nested fields, dictionaries and binary metadata staged |
| Statement option API / ingestion | All typed callbacks and four ingestion modes staged; scoped native-name/temporary-shadow/row-count/error journeys pass. All 64 frozen upstream cases pass; broader type/native/failure coverage remains open |
| Connection metadata | All six callbacks implemented; frozen object-list SQL retained, prescribed dense unions/NOT NULL metadata validated by PyArrow and C++ comparisons |
| Partitions / Substrait | Exact frozen unsupported partition callbacks staged; initializer must preserve C++'s untouched Substrait slot |
| ADBC 1.1 error API | Detail count/get/from-stream implemented; owned/borrowed message lifetimes, deferred errors and cancellation type details verified in scoped journeys |
| Initializer | Exported; 1.0/1.1 callback wiring matches frozen C++; unsupported versions, untouched slots, legacy driver/error prefix canaries and guard pages pass |
| Packaging / driver manager | Pinned Apache Python ADBC driver manager loads the release library; all 64 frozen upstream cases pass; packaging and all selected DuckDB clients remain open |

The temporary dead-code allowance was removed when initializer wiring made all callbacks
reachable. Production implementation is Rust. The C++ files in `crates/capi/tests/` are
public-ABI reference probes, not shipped engine dependencies. Checkpoints below describe
their recorded snapshots; the newest checkpoint supersedes old pending statuses.

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

## Ingestion and native appender checkpoint, 2026-10-24

The continuation implements all typed option callbacks and stream ingestion in create,
append, replace and create_append modes, including temporary target handling, explicit
catalog/schema resolution, affected-row counts and frozen failure statuses. Ingestion
uses the Rust engine's C appender. Native byte names now also cross table description,
selected appender column and query-appender SQL/name/CTE interfaces. Existing text Rust
QueryAppender callers retain their API; native clients use `for_query_bytes`.

**Eleven signed development-profile library tests pass**, none ignored: nine C API
library tests and two optimizer tests. The C API count includes eight ADBC regressions
and the existing Arrow test. Five C++ public-driver-ABI journey families pass: stream
lifetime, transactions/parameters, bound parameters, ingestion modes and typed options.
Both engines pass the expanded maintained seven-name duckdb.h journey, now including
table descriptions, selected-column appending and query appending.

**1,747/1,747 full public C ABI appender journeys match C++**, all producing sum 49.
Each exercises catalog/schema/table/column/CTE names, description metadata, selected
column append and query appending. Names are derived from the existing native-byte
corpus using C-string NUL termination and nonempty prefixes. This verifies actual
in-memory catalog and appender operations, not filesystem names or arbitrary VARCHAR
values. Python ctypes is the comparison harness, not a shipped language binding.

The first expanded C test failed in the direct selected-column append path. Join
statistics still compared `expression.to_string()` to `count_star()`, panicking when a
bound name contained invalid UTF-8. Comparing native `to_bytes()` preserves the frozen
C++ comparison and fixes the failure; the new optimizer regression and full ABI matrix
pass. The failed output and backtrace are retained. A separate C++ double-option
comparison rejected an assumed signed NaN spelling: this Mac's C++ returns `nan` for
both signs. The Rust setter now uses the same native `%f` conversion, including the
finite, infinity and negative-zero cases. The failed comparison/diagnostic are retained.

The rebuilt release library passes three affected upstream C API files, 16 test cases
and 615,708 assertions; existing C++-internal harness exclusions are unchanged. Final
C API/runtime/optimizer all-target Clippy with warnings denied and workspace compilation
are tracked in the checkpoint. Runtime test cleanup removes redundant generic borrows;
the native-path fixture now builds SQL/path suffixes from OS bytes rather than decoding
its paths into String. No assertion was removed to obtain acceptance.

The initializer is still unexported. Metadata/statistics, error details/deferred stream
errors, catalog/schema option Value-boundary semantics, complete upstream ADBC tests,
initializer prefix bounds and real driver-manager/client loading remain open. These
results do not complete Python or any other selected language package, the existing
ENUM exception gap, or final corpus/shell/storage/WAL/performance acceptance. Table-block
release-on-unpin and the accepted lower-memory policy remain unchanged.

New evidence under the same external SSD directory: `adbc-ingest-checkpoint.json`,
`adbc-ingest-tests-3-summary.json`, `adbc-ingest-cpp-3.log`,
`capi-native-appender-summary.json`, `capi-native-appender-{cpp,rust}.json`,
`adbc-ingest-names-rust-2.log`, `adbc-ingest-names-cpp.log`,
`adbc-ingest-upstream-tests-2.log`, `adbc-ingest-clippy-6.jsonl` and
`adbc-ingest-workspace-check-3.jsonl`.

## Metadata, initializer and real client checkpoint, 2026-10-24

The initializer now exposes the complete frozen callback table for ADBC 1.0 and 1.1.
Only slots present in the negotiated version are written; the same private/release/
Substrait slots C++ leaves untouched remain untouched. Export inspection now finds
**547 duckdb_ symbols**, matching the reference with no missing or additional symbols.
The Rust dylib still has no C++ DuckDB engine dependency.

Object metadata retains the four exact frozen SQL templates and byte-preserving filter
substitution. Table schemas/types use the same query path. Catalog/schema option reads
now query the engine, preserving the C++ VARCHAR validation boundary. GetInfo/statistics
build the required owned dense-union arrays, nullable and NOT NULL flags, empty children,
list/map offsets and all-NULL column-name buffers. Statistic names are the prescribed
empty result; approximate row estimates remain approximate even when exact is requested,
as in C++. Error details retain native messages and copy the frozen 43 error type names.
A stream owns its borrowed error message and rich detail payload until release.

**13 signed C API library tests pass**, none ignored. Both signed public-ABI probes pass
seven journey families, including metadata/rich/deferred errors and mmap guard pages
that make the legacy 1.0 driver/error tails inaccessible. Separate signed connection
and statement cancellation probes pass on both engines, then execute a new query;
both report stream status 9 and detail type Interrupt for this frozen error path.

A native arm64 Python 3.13 environment on the SSD pins adbc-driver-manager 1.4.0,
PyArrow 19.0.1 and typing-extensions 4.16.0. The actual Apache driver manager loads the
requested library and passes **12 client journey comparisons**: parameters, nested/
decimal/temporal/blob/NULL data, full Arrow validation, ingestion, schema/types/object
metadata at every depth, rich errors, and commit/rollback. Loaded-image checks show
exactly the requested DuckDB library for each process; there is no alternate engine.
This is ADBC Python-client evidence, not implementation of DuckDB's full Python API.

A further **13 direct ADBC-to-PyArrow metadata cases match C++** with full Arrow
validation: default/empty/unknown/repeated info codes, catalog/schema/table-filtered
statistics across multiple catalogs and schemas, no-match results, exact/approximate
requests and empty statistic names. The older Python ADBC package does not expose
statistics methods, so those cases use the public driver function table and import its
Arrow C stream into PyArrow.

The reference build reports v0.0.1, while Rust reports its declared v1.5.6. Both GetInfo
version fields are independently checked against each library's own core version.
Only these verified build-identity strings are normalized in comparisons; raw reports
and the initial 11/12 comparison are preserved. No functional mismatch is hidden.
The first deferred-error test also failed because its query errored before returning
a stream. The nonconstant late-error query now verifies buffered batches precede the
error; the same query passes against C++ through the public ABI.

The current release library passes **all 30 upstream C API harness files**, 155 test
cases and 814,544 assertions in this run. Existing C++-internal exclusions are unchanged.
C API all-target Clippy with warnings denied and full workspace/all-target compilation
pass. These do not substitute for the still-unrun 64-case upstream ADBC suite, fuller
native filter/error and lifetime matrices, client packaging, Python/all selected DuckDB
clients, existing ENUM failure semantics, or final corpus/shell/storage/WAL/performance
acceptance. No table-cache retention or accepted speed-memory policy changed.

Evidence on the external SSD: `adbc-metadata-checkpoint.json`,
`adbc-metadata-tests-5-summary.json`, `adbc-metadata-{rust,cpp}.log`,
`adbc-cancel-{rust,cpp}.log`, `adbc-python-comparison-2.json`,
`adbc-arrow-metadata-summary.json`, `adbc-python-requirements.txt`,
`adbc-metadata-upstream-capi-summary.json`, `adbc-metadata-clippy-2.jsonl`,
`adbc-metadata-workspace-check-1.jsonl`, `adbc-metadata-exports.json`.
Client behavior is exercised using Apache's [ADBC Python API](https://arrow.apache.org/adbc/main/python/api/adbc_driver_manager.html),
with the installed pinned API inspected locally rather than assuming current docs match it.

## Full frozen upstream suite checkpoint, 2026-10-24

**64/64 upstream ADBC cases pass on Rust and C++, with 3,040 assertions on each.**
`scripts/adbc-tests/run.sh` compiles the frozen test file, driver manager and nanoarrow
helpers. All cases/assertions remain; source/adaptation hashes and assertion counts
are checked. Two private C++ database helper bodies, the private autocommit inspection
and private error initializer are adapted to the selected implementation. The Rust
executable never compiles the C++ reference bridge or links a C++ DuckDB engine.

The optional `rustdb-capi/adbc-test-api` feature exposes three borrowed-handle/test
initializer functions solely for this suite. The test dylib lives in its own SSD
directory with an @rpath identity and signature. Both runners verify the requested
engine image is the only DuckDB library loaded before/after the suite. The normal
shipping build is restored, signed and checked to expose **547 duckdb_ functions and
zero test bridge functions**. No test feature is required by clients.

Reference calibration caught adapter issues before Rust acceptance. A borrowed proxy
preserves driver-manager release identity when the public Arrow scan API changes its
private copy. The nested value helper uses the engine's VARCHAR cast, since legacy
C result getters cannot fetch nested values. Arrow comparisons retain logical type
parameters and use SQL EXCEPT ALL for duplicate/NULL/numeric semantics. The same
adapter passes the complete reference suite; no failing assertion or case was removed.

The first Rust run passed 63/64, failing dynamic PIVOT preparation with `Cannot plan
statement of type MULTI!`. C++ preprocesses SELECT-containing multi-statements into
their component statements. The shared text/byte connection preprocessing now does
this before extraction/preparation, preserving PIVOT's unwrapped enum-create + SELECT
behavior. Both shipping C APIs pass the maintained extracted/prepared/executed PIVOT
probe. Two signed runtime regressions pass, including native invalid-UTF8 identifiers.
The initial failing run/library and harness calibration evidence are retained.

The normal shipping library passes **all 30 upstream C API files**: 155 cases and
706,158 assertions in this run (pending-task loops make assertion totals run-dependent).
Existing C++-internal exclusions remain unchanged. Thirteen signed C API unit tests
also pass, none ignored. Default C API/runtime all-target Clippy, test-feature C API
all-target Clippy, and complete workspace/all-target checking pass.

This closes the frozen upstream ADBC suite gate. It does not complete native/fuzz/type/
packaging matrices, DuckDB's Python API or other selected clients, the existing ENUM
failure gap, raw PEG/value codecs or final full engine/corpus/shell/storage/WAL and
speed-memory acceptance. Non-SELECT multi/pragma extraction and transaction-policy
preprocessing remain a separate broader core audit; this fix covers the PIVOT/SELECT
unpacking branch and retains existing handling elsewhere. No table-cache retention or
accepted performance policy changed.

Evidence: external `adbc-upstream-checkpoint.json`, `adbc-upstream-summary.json`,
`adbc-upstream-{cpp,rust}-1/test.log`, source manifests, test/shipping libraries and
symbol/dependency records, `adbc-pivot-{cpp,rust}.log`,
`adbc-pivot-runtime-tests-1-summary.json`, `adbc-upstream-capi-unit-tests-1-summary.json`,
`adbc-upstream-shipping-capi.log`, `adbc-upstream-clippy-{default,test-api}-1.jsonl`,
`adbc-upstream-workspace-check-1.jsonl`. Harness details: `scripts/adbc-tests/README.md`.


## Current source-5 native refresh (2026-10-27)

The complete frozen native suite passes again on current Rust and frozen C++:
64 cases and 3,040 assertions each. Ordered case starts and source/adaptation hashes match;
the selected engine image is verified before and after both suites. The optional bridge
library is preserved separately. The default shipping build was restored, signed and
verified to expose 547 C API functions and zero test hooks **before** the harness ran.
Current source has 908 verified build inputs; all hashes remain stable.

Evidence: external `import-preprocessing-20261027/adbc-native-1/verification.json`,
per-engine complete logs, manifests, test-library identity and shipping restoration receipt.
The installed Python/ADBC file journeys and two import/rollback controls also pass on both
Arrow environments. Non-SELECT import/PRAGMA preprocessing is now covered by five exact
public extraction contexts and seven complete prepare/execute C API journeys, including
query-owned rollback after a failed extracted prepare.

The current full 4,870-file corpus is running. This gate does not establish final whole-port
performance, other platforms, or all broader client/backend/lifetime coverage.

## Source-8 full native checkpoint (2026-10-27)

Fresh native Rust and C++ runs pass all 64 frozen upstream cases and 3,040
assertions each, with identical ordered case IDs and adapted-source hashes.
The harness records actual native loader paths and checks exactly one selected
engine before and after execution. Every upstream assertion is retained; only
two private helper bodies and one private autocommit read use the recorded test
bridge adaptation. Feature-enabled all-target Clippy passes with warnings denied.

The bridge-enabled Rust image is copied and signed separately. Before either
harness runs, the default shipping library is restored, signed and verified at
547 C API exports with zero test hooks. The immutable shipping image remains
unchanged; the post-run audit repeats those checks. Test-only bridge symbols
are never added to the shipped Python module or Swift distribution.

Evidence: external `batch-copy-memory-20261027/adbc-native-1/{verification.json,
shipping-verification.json,test-lib-manifest.json,runner-manifest.json,
runner-adaptation.patch,clippy-terminal.json,rust-suite/test.log,cpp-suite/test.log}`.
The shipping library SHA-256 is
`2c96baa818920f6be19daaebea5ed3e3cbcf833a0a308139329ff0184bebca0c`.
Installed Python/ADBC handoffs on Arrow 19/21 also pass with one Rust image each;
these package receipts remain separate from the native 64-case gate. Broader
applicable platform/version matrices and whole-goal final acceptance remain open.
