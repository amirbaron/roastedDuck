# postgres_scanner: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-postgres/tree/318dabb2474fc3789b0199301a2e661e19c1b4fc

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/postgres_scanner.cmake`.
The adjacent `upstream.json` inventories 152 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

ATTACH with postgres/postgres_scanner, remote catalogs/transactions, scans/filter pushdown, writes/DDL; postgres_scan, postgres_scan_pushdown, postgres_attach, pg_clear_cache, postgres_query, postgres_execute, COPY (FORMAT postgres_binary), read_postgres_binary, postgres_configure_pool, hstore functions; postgres and rds secret types, config provider and all pg_* settings. Inventory source names below is authoritative over this prose. No libpq dependency; implement protocol and catalog integration in Rust.

## Acceptance

Run unchanged upstream SQL against C++ DuckDB 1.5.6 and Rust. Keep missing-service
prerequisites explicit; skips do not qualify a feature. Add differential controls
for auth, scope, cancellation, transactions and errors where applicable. Use local emulator/disposable-service
integration in addition to protocol fixtures, as authorized by the owner on
November 14. Record live-cloud behavior separately as unverified until exercised. Run scripts/conformance.sh
after integration merges, then verify normal Python and Homebrew packages.
No new C/C++ dependencies, binary adapters, default-off gates or public plugin ABI.

## Status

Contract recorded; not a claim that new functionality passes acceptance.

## Attached scan checkpoint (November 15)

Attached tables now execute bounded COPY OUT scans with CTID page tasks, pooled
parallel workers and shared PostgreSQL snapshots for repeatable-read/serializable
transactions. READ COMMITTED omits snapshot sharing. Version <14, text mode and
nonpositive relpages disable splitting. Multiple scans reserve/materialize the main
connection according to the pinned optimizer rules; auxiliary leases return on
completion/drop. Worker statement/idle timeouts use real server settings.

LIMIT/OFFSET pushdown disables splitting. Supported comparison/IN/NULL/struct
filters are translated to PostgreSQL SQL, with upstream string collation and blob
literals. Optional join hints cannot become mandatory predicates. Ordinary rowid
reads yield NULL; a physical-planning callback enables CTIDs/materialization for
provider DML even with the optimizer disabled. Provider scans now retain their
cardinality callback rather than falling back to the native-table path. Query logs
register the structured PostgresQueryLog type and capture actual executed queries.

Receipt: .context/artifacts/popular-extensions/postgres-parallel-verification.json.
Latest Rust full 152: 134 PASS/11 FAIL/7 SKIP (postgres-upstream-parallel-all-2).
C++ reference: 145 PASS/7 SKIP (postgres-upstream-parallel-all-1). All originals stay
unchanged. The first scan discovery was 132/13/7; the final build fixes partitioned
EXPLAIN cardinality and filter pushdown with no other status changes. All 9 selected
scan/pool originals pass in the final Rust full run; C++ passed that cohort in
postgres-upstream-parallel-4. All 12 paired scan controls pass, including actual
worker/snapshot/timeout/filter log assertions, tight-pool joins and optimizer-off
mutations. All 27 attachment, 22 mutation and 6 API cancellation regressions pass.
All 272 native tests across 41 signed targets pass with TZ=UTC; strict Clippy run 2 passes.
CLI SHA starts f6f97a144265; SLT starts fb4484d7ed46. All owned fixtures stopped.

Initial control 1 used CLI shell log storage instead of memory and exposed an
unsupported optional join-filter hint. Control 2 corrected both; control 3 verifies
the final planner build. Failed receipts are retained. No new dependencies or gates.

Next: 11 original failures remain: main-schema resolution; created composite-type
field binding; postgres secrets and secret storage; four staleness/cache tests;
information_schema introspection; two transaction error-compatibility tests. Legacy
postgres_scan/postgres_scan_pushdown and postgres_query still need unified scan/
optimizer/logging/settings integration. Remaining authentication/prerequisites and
all provider/platform/package gates stay open. Full core is still frozen 83d8da21
and predates later PG work; final current-source core qualification is required.
No new PyPI/Homebrew release; the all-five goal remains active and incomplete.

## Connection pool checkpoint (November 15)

Primary transaction sessions now use a per-catalog Rust pool. Force acquisition can
exceed the cache limit; try acquisition fails immediately at capacity, and wait
acquisition uses a fixed deadline. Leases are returned at transaction completion,
with health checks, DISCARD ALL and driver type-cache invalidation. Open/failed
transactions are rolled back first using the protocol's transaction status. Temporary
objects and session settings therefore do not leak between borrowers.

The implementation includes idle/lifetime expiry, background reaping, the pinned
single thread-local cache slot and its thread-exit return, resizing and shutdown.
postgres_configure_pool applies live options and reports real counters. Global
pg_pool_* defaults affect future attachments; deprecated pg_connection_limit and
pg_connection_cache callbacks update attached pools and propagate their setting
aliases. Parallel scan acquisition still needs its engine integration.

The generic pool source is database-connector commit
0a8505f775dae7bb30edd37f848d5975bf72884e; ten source blobs and their verified Git hashes
are retained in postgres-pool-source under the artifact directory. The Rust driver
patch remains limited to client.rs/query.rs, with unchanged licenses/dependencies.

Evidence: postgres-pool-verification.json, postgres-pool-backend-4,
postgres-pool-control-3 and postgres-upstream-pool-4. All28 real-server lifecycle
checks,16 paired SQL cases and five unchanged originals pass. The original files
are pool options/configure, autoload, multi_join and concurrent_queries. The slow
pool and early-release originals remain unqualified until parallel scan/settings
work is complete. Prior27 attachment/22 mutation/16 DDL/39 codec comparisons and
six API cancellation modes pass. All272 native/runner tests across41 signed targets
and strict Clippy5 pass. All owned services stopped.

Fixture-only secret-directory isolation is applied before opening the oracle/Rust
databases, without editing original SQL. Platform-specific hardware-derived defaults,
complete PG inventories and the final core/provider/package/publication gates remain
open. This milestone is not a full extension acceptance claim.

## Binary COPY files checkpoint (November 15)

COPY TO/FROM (FORMAT postgres_binary) and read_postgres_binary are registered in
normal builds. The reader requires a columns struct and accepts buffer_size
(default32 MiB); it follows the pinned header/row/footer and refill behavior,
including rows that fit the buffer only after the header is consumed. It verifies
bounds and column counts before decoding. Prepared executions open fresh readers,
and repeated EOF reads do not touch the input again.

The file writer preserves the pinned type whitelist, scalar/temporal representation,
full decimal scale groups, nested array/composite headers and OID restrictions,
NULL-byte replacement and GEOMETRY/EWKB behavior. Files are uncompressed regardless
of filename suffix, as in the pinned file-open flags. Existing filesystem adapters
provide local/registered reads and writes, file/home path handling, logging and
external-access checks. No PostgreSQL server is needed for these file functions.

The server round trip also corrected timestamp epoch wrapping and date write-range
errors in the shared wire codec. Invalid NUL replacement settings now produce the
pinned Binder error. These changes retain the Rust-only implementation and add no
new dependency or default-off gate.

All seven original binary files and the original date/timestamp files pass on both
engines. Sixty paired controls pass, including11 byte-identical writer families and
both read directions, plus four I/O controls with real loopback HTTP GETs and
access-denial checks. Three native reader tests cover fragmented input, fused EOF,
row-size boundaries and malformed layouts. All219 tests across39 signed targets
and strict Clippy5 pass; prior27 attachment/22 mutation/16 DDL/39 codec SQL controls
remain green. Receipts are postgres-binary-verification.json,
postgres-upstream-binary-3, postgres-binary-control-4, postgres-binary-io-1 and
postgres-binary-*-regression-1 under `.context/artifacts/popular-extensions/`.
Owned servers stop after testing. Broader malformed-layout equivalence is not
claimed from the tested fixtures.

Complete PG, provider/platform, final core/package and publication gates remain open.

## DDL, export and hstore checkpoint (November 15)

roastedDuck's compatible implementation now supports server-owned index creation,
IF NOT EXISTS and index listings; CREATE/REPLACE/DROP VIEW; and server CHECKPOINT
through the active transaction. The binder retains resolved remote schemas and
uses provider DDL instead of native ART storage. Quoted identifiers retain their
PostgreSQL spelling, including distinct objects differing only by case.

CREATE/DROP update the affected cache entries and retain unrelated definitions.
Schema table/type/index name sets are captured together. Newly created table
entries keep defaults and logical aliases such as JSON without changing decoder
layouts. The pg_array_as_varchar setting does not rewrite a freshly created array
column's declared type; explicit cache clearing restores the normal reload mapping.
Full staleness/identity-swap/rebinding behavior remains pending.

Database copy and export use provider enumeration. Copy preserves resolved type
infos; temporary metadata transactions are rolled back even after errors. A filtered
schema export/import round trip of newly created tables and views passes. Pinned
quirks remain: an export containing existing composite types can write aliases that
fail on import, and mixed-case index column expressions can fail after PostgreSQL
folding. These are verified oracle outcomes, not claims of broader export acceptance.

The hstore functions preserve byte parsing, duplicate-key lookup, JSON order and
escaping, NULL values and exact tested syntax-error positions. The differential
matrix covers 200 inputs across17 keys, NULL/control-byte cases and9 malformed inputs.

Final receipts under `.context/artifacts/popular-extensions/`:
`postgres-ddl-verification.json`, `postgres-ddl-control-8`,
`postgres-hstore-control-3`, `postgres-upstream-ddl-3`, and the four
`postgres-ddl-*-regression-2` cohorts. All16 DDL/export comparisons, nine unchanged
originals on both engines,27 attachment/22 mutation/7 metadata/39 codec SQL controls,
40 targeted native SQL files and216 tests across39 signed targets pass. Strict
all-target Clippy6 passes. Owned servers stopped. No new dependency or default-off gate.

The preceding frozen83d8da21 full core run has zero regressions/status changes:
4,844 PASS /33 SKIP /same20 documented FAIL. It predates this milestone. Remaining
PG functionality, complete extension inventories, final core/platform/package
qualification and PyPI/Homebrew publication stay open.

## Metadata and full-inventory discovery (November 15)

The first frozen full-152 run produced C++ 144 PASS / 7 SKIP / 1 FAIL and Rust
95 PASS / 7 SKIP / 48 FAIL / 2 CRASH. The C++ failure is a fixture permission for
server-side COPY; seven skips retain their original missing prerequisites.
Original mode-skip directives remain in place. This is discovery, not acceptance.
Raw verdicts and the two crash reports are in `postgres-upstream-all-1`.

Internal catalog enumeration now routes schema/table/type listing through the
provider, preserving errors and owned entry lifetimes. System table functions use
those entries without creating native storage. PostgreSQL metadata also supplies
its catalog type and internal schema flags. Column names are deduplicated for SQL
binding while wire queries retain PostgreSQL names. Type lookup is case-insensitive;
newly created table entries retain default expressions until cache invalidation.
Index enumeration and complete cache behavior remain incomplete.

The two composite-field crashes came from incorrectly treating remote table
identity as native extracted-field scan capability. That optimization is now
restricted to native storage. All seven paired metadata controls and nine original
files pass, including both previously crashing originals, comments, DESCRIBE,
case-sensitive columns and enums. Receipts: `postgres-metadata-control-1` and
`postgres-upstream-metadata-1`. All 26 signed runtime library tests and strict
catalog/optimizer/runtime all-target Clippy pass. The original C++ binary COPY
file passes with a test-scoped server-file role, revoked in finally; receipt
`postgres-binary-fixture-oracle-1`. Full current-source conformance remains pending.

## Mutation and ALTER checkpoint (November 15)

Remote UPDATE, DELETE and MERGE now execute through internal mutation state.
Updates stage values and CTIDs in PostgreSQL temporary tables; deletes flush CTID
lists at the pinned threshold. MERGE buffers its input through existing spillable
storage, then uses the native action classifier with interleaved provider sinks.
COPY inserts complete per MERGE chunk, updates apply on finalization, and delete
batches retain their state across chunks. Counts follow input rows as upstream does.
RETURNING and SET DEFAULT retain the pinned restrictions, including the zero-row
UPDATE DEFAULT case. ON CONFLICT is rewritten to MERGE; it is supported through
PG key metadata rather than rejected using the older PlanInsert check.

PK/UNIQUE metadata maps constraint attnums onto live columns, preserving correctness
after DROP COLUMN. Remote ALTER supports rename table/column and add/drop column;
other ALTER forms retain the pinned rejection. The backend performs the schema
change and invalidates its cache; native table storage is not reconstructed.

Twenty-two paired controls pass, as do nine unchanged originals on both engines:
update, delete, merge, on_conflict, on_conflict_dropped_column, alter,
alter_add_column_default, alter_rollback and large_delete. The million-row case
runs with its unchanged data and expected results. attach_delete's later mode-skip
remains in place and does not qualify its skipped suffix. Six API interruption
modes pass, including actual server sleep/cancellation and rollback of update,
delete and merge effects. Servers stop after verification.

Receipts: postgres-mutation-control-4, postgres-upstream-mutation-2 and
postgres-mutation-interrupt-1 under `.context/artifacts/popular-extensions/`.
Earlier failures (unbound ADD COLUMN type and absent-table DROP schema) remain.
All 216 native tests across 39 signed targets pass with TZ=UTC; strict final
Clippy 9 passes. Receipts: postgres-mutation-unit-results.json and
postgres-mutation-clippy-9.log. The full PG and five-extension/release gates remain open.

## Real attachment and insert checkpoint (November 15)

The runtime now attaches a PostgreSQL-owned catalog with real table entries and
transaction-owned sessions. It implements reads, postgres_query/postgres_execute,
cache clearing, schema filters, isolation/read-only options, CREATE table/schema/
type, DROP and INSERT/CTAS through remote DDL and COPY. Table data is not copied
into native DuckDB tables. Unread cursor rows and insert inputs use the engine's
existing spillable column storage when their connection must perform another
operation. Replacing CTAS reads the old relation before DROP/CREATE.

The final attachment controls pass all 27 paired cases, including replacement and
ignored CTAS, self-insert, overlapping queries, defaults, enums, binary/text COPY,
NUL replacement, permissions and transaction rollback. Thirty-nine value/type
comparisons now run entirely through Rust SQL, including attached tables and query
functions. Fifteen unchanged upstream files pass on both engines: the prior seven
scanner files plus types, arrays, query, query_params, query_error, uppercase-name
creation, null-byte handling and binary-COPY settings. API interruption controls
observe actual server PgSleep and verify cancellation of legacy/attached reads and
COPY inserts, zero committed inserted rows and no idle remote transaction.

Text protocol is actual PostgreSQL wire format zero. A small vendored Rust driver
patch adds a raw-text method; its original MIT/Apache licenses remain. Only
src/client.rs and src/query.rs differ from tokio-postgres 0.7.18. No package version
or native dependency is added. SQL query parameters have their own pinned encoder
(VARCHAR text; tiny/utiny/small two bytes; usmall/int four; uint/big eight; float and
double; other non-NULL types rejected). Binary COPY's rejection of parameters is
preserved through the server's actual error. COPY uses its separate value encoder.

Receipts under `.context/artifacts/popular-extensions/`: postgres-attach-control-5,
postgres-codec-runtime-2, postgres-upstream-attach-2, postgres-interrupt-control-2,
postgres-driver-patch-audit.json and postgres-text-driver-audit.json. The final
wire probe and 26 paired legacy-scan controls also pass (postgres-wire-control-4
and postgres-scan-control-3). Earlier
failures are retained, including nonfirst-column projection, weak planner-context
lifetime and replacing-CTAS ordering regressions. Supplemental fixture mistakes
(parameter protocol selection and a reserved SQL alias) were corrected against the
oracle; original test SQL and expected results were not changed.

Full core conformance on the preceding hook commit d8f61a2f passes its ratchet:
4,844 PASS / 33 SKIP / the same 20 documented FAIL, zero status changes and no
baseline changes (postgres-hooks-conformance-verification.json). That frozen run
does not qualify the later attachment implementation. Fresh integration and
package/platform gates remain before publication.

Still required: UPDATE/DELETE/MERGE and remaining DDL; complete catalog listings,
cache/staleness/rebinding, filter/parallel scan behavior, pooling, secrets/RDS,
OAuth/CRL/options and other remaining upstream surfaces; all 152 originals and
supplemental controls; full five-extension and publication acceptance. All 216 unit/integration tests across 39 signed targets pass with TZ=UTC; the
existing UTC-rendering assumption in one persistence test is recorded with its
local-timezone failure (postgres-attach-unit-results.json and
postgres-attach-unit-utc-results.json). Strict all-target Clippy passes. No release
has been published from this checkpoint.

## Internal engine hooks checkpoint (November 15)

The catalog now accepts engine-owned Rust metadata and table backends. Fallible
lookups preserve provider errors; table entries bind provider scans without native
DataTable storage. Catalog-specific default schemas flow through USE/search paths.
Native-only optimizations are gated by native storage rather than table identity,
so remote providers retain predicates, sampling and projections they do not accept.
Transaction participants are owned per catalog, reused within a transaction and
committed/rolled back outside the native manager lock. Read-only participants finish
before the database allowed to write; this is not distributed two-phase commit.

The 26 signed runtime tests include synthetic-provider end-to-end scan/lookup
checks and participant lifecycle failures. Strict core/runtime Clippy passes.
Receipts: postgres-catalog-hooks-tests-2.{log,json} and
postgres-catalog-hooks-clippy-6.log. These tests qualify internal hooks only; they
are not PostgreSQL service acceptance. The actual PG catalog and DDL/DML integration
remain to be implemented, and fresh full core conformance is pending.

## Codec and legacy scanner checkpoint (November 15)

Normal builds now register bundled `LOAD postgres` / `postgres_scanner`,
`postgres_scan`, `postgres_scan_pushdown` and legacy `postgres_attach`.
The scans stream projected binary rows through Rust codecs. The legacy attach
function creates views using a separate DuckDB connection, matching the pinned
source; it is not the implementation of SQL `ATTACH ... TYPE postgres`.
Actual filter pushdown and remote catalog/transaction/mutation hooks are pending.

The codecs distinguish table metadata from `postgres_query`'s narrower OID
whitelist. They implement numerics, temporal types, UUID byte order, arrays,
enum/composite values and PostgreSQL geometric primitives. Unknown scalar/array
element types use PostgreSQL-side VARCHAR/VARCHAR[] casts. Binary parameter
encoding follows the DuckDB input type. Exhausted row and COPY streams stay
exhausted; interruptible row reads send PostgreSQL's cancellation request.

The signed development image passes 39 value-and-logical-type comparisons:
13 actual Rust SQL scans and 26 backend table/query-mapping controls, each compared
to C++ 1.5.6 on PostgreSQL 17.11. Seven parameter types also round-trip through the
real server, including DECIMAL(38,38), UUID and nullable multidimensional arrays.
Twenty-five paired SQL controls pass projections, batching, joins, prepared reuse,
array shape errors, quoted names, settings and legacy view-creation behavior.
The Rust API interruption probe waits for `pg_stat_activity.wait_event='PgSleep'`,
interrupts the query, observes it stop on the server, then reuses the connection.
All owned servers stop after testing. Strict all-target backend/runtime Clippy
and all 23 signed runtime library unit tests pass.

Two supplemental assumptions were corrected against the actual pinned source
and oracle, without changing any upstream SQL: direct legacy scans/attach do not
check `enable_external_access`; legacy `PostgresBind` leaves parser `type_config`
at its defaults, so numeric NaN remains NULL after `pg_numeric_nan_as_null=false`.
The new runtime preserves those behaviors. SQL ATTACH's distinct access checks
remain required. Legacy attach also accepts but ignores `suffix` and discards
per-view creation errors, as the pinned `AttachFunction` does.

Receipts under `.context/artifacts/popular-extensions/`:
`postgres-codec-control-4`, `postgres-scan-control-2`,
`postgres-scan-clippy-4.log`, `postgres-codec-unit-tests.log`,
`postgres-wire-control-3`, `postgres-upstream-scanner-1`, and
`postgres-runtime-unit-tests.{log,json}`. Earlier failures
remain in control1 directories; they include an exhausted-row-stream defect fixed in the backend and the two
incorrect supplemental assumptions above. Seven unchanged originals also pass
on both engines: bug63, count_star,
decimals (with the upstream slow prerequisite enabled), interval, missing_table,
nulls and ssl. The four malformed-codec tests and updated wire probe pass.
These checks do not qualify the 152-file suite or SQL ATTACH/query/write/pool/authentication integration.

## Initial oracle fixture

The pinned `database-connector` submodule is
`0a8505f775dae7bb30edd37f848d5975bf72884e`. A basic reference fixture is executable
with `scripts/postgres-tests/oracle_control.py`. It creates a fresh, loopback-only
PostgreSQL cluster under the supplied artifact directory, verifies remote reads,
writes, rollback, query pushdown and a local/remote join, then stops the server.
The first run passes on C++ 1.5.6 and PostgreSQL 17.11; Rust was not tested because
the connector was not implemented at that checkpoint. This does not replace the 152 original SQL
files or pool/TLS/authentication/concurrency acceptance.


## Wire backend checkpoint (November 15)

`crates/postgres` adds a Rust transport foundation using tokio-postgres0.7.18 and
Rustls/ring. It does not link libpq or implement a C++ engine adapter. The backend
has streaming binary results and COPY, raw parameter bytes, column OIDs/type
modifiers, owned runtimes and query cancellation. Connection/environment/service/
password-file handling and basic TLS modes are present. SQL registration and
remote catalog/transaction/mutation hooks are not yet integrated; this is not
`LOAD postgres` support or completed compatibility acceptance.

The signed wire probe passes against a disposable TLS/SCRAM PostgreSQL17.11 server
using a non-superuser database-owner role. It verifies actual TLS, binary values,
server rejection of a mismatched binary parameter, exact COPY round trips,
rollback, cancellation, operation and teardown within another Tokio runtime,
SSL modes, CA-versus-hostname verification and wrong-password rejection. The C++
TLS control passes on the same server. Strict all-target backend Clippy passes.
The dependency audit preserves all existing locked versions;41 packages are added,
without new native library packages. Transitive WASM updates were pinned back.

The reusable fixture restores a seeded postgresscanner database before each file.
TPCH/TPCDS generators are explicitly loaded from owned, signed1.5.6 oracle artifacts
with automatic loading/downloading disabled. Original setup SQL is checked against
the pinned archive; client-side COPY avoids server-file privileges. Four unchanged
originals (types, arrays, SSL, uppercase-name creation) pass on C++ with the final
CA/server chain; servers stop after both runs. Tests remain unmodified, including
upstream mode-skip directives.

Receipts under `.context/artifacts/popular-extensions/`: postgres-wire-control-2,
postgres-upstream-fixture-2, postgres-dependency-audit.json,
postgres-backend-clippy-final-2.log, and postgres-dataset-oracle-1.
Still required: SQL/catalog/transaction/writes, full codec/options/pool behavior,
OAuth/CRL and remaining authentication parity,152 originals and supplemental
controls, full integration conformance and package/platform/publication gates.
