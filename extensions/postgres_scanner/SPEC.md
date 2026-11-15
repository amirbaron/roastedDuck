# postgres_scanner: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-postgres/tree/318dabb2474fc3789b0199301a2e661e19c1b4fc

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/postgres_scanner.cmake`.
The adjacent `upstream.json` inventories 152 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

ATTACH with postgres/postgres_scanner, remote catalogs/transactions, scans/filter pushdown, writes/DDL; postgres_scan, postgres_scan_pushdown, postgres_attach, pg_clear_cache, postgres_query, postgres_execute, postgres_binary_copy, read_postgres, postgres_configure_pool, hstore functions; postgres and rds secret types, config provider and all pg_* settings. Inventory source names below is authoritative over this prose. No libpq dependency; implement protocol and catalog integration in Rust.

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
