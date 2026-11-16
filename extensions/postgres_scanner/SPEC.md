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

## Version, snapshot and CRL checkpoint (November 15)

Version detection now uses the pinned SELECT version()/RDS-settings probe for
both ATTACH and standalone scans, including UNKNOWN fallback on probe failure.
Numeric component parsing, Aurora/Redshift precedence and pre-8.3 enum visibility
follow upstream. Redshift forces text reads. Snapshot-export errors disable
sharing without resetting an aborted server transaction, so the later scan reports
the server's transaction failure as upstream does. The old-version recovery query
is retained for the pinned version-dependent snapshot branch.

File-based v2 CRLs now use Rustls chain, signature, issuer, revocation and expiry
verification; thisUpdate is checked separately because webpki omits it. verify-ca
omits only hostname checking, while verify-full retains it. Existing root files
also enable chain verification in prefer mode, following the pinned source.
Missing/unreadable/invalid-PEM CRL files do not activate revocation checking;
loaded malformed or unsupported DER fails explicitly. A certificate-only CRL file
requires a CRL and fails. This is not a claim of every OpenSSL CRL format/option:
v1/delta CRLs, CRL directories and system-root option behavior remain unqualified.

The rustls-webpki 0.103.17 patch permits the optional extensions field to be absent
in a v2 CRL, as libpq accepts. Signed bytes and verification algorithms are
unchanged. Registry archive/license audit: only src/crl/types.rs differs among
33 audited source/manifest/license files. The new direct x509-cert dependency was
already locked; no package version or C/C++ dependency was added.

All 9 paired version/snapshot controls and 16 paired CRL controls pass, including
failed probes, Redshift text mode, revoked/expired/future/bad-signature/wrong-issuer
and critical-extension rejection. Parallel12, cache13, unified-scan21, OAuth48
records (including the unchanged original on both engines) and debug8 pass.
The full original PG inventory remains 145 PASS / 7 SKIP / 0 FAIL; its separately
qualified OAuth prerequisite remains explicit. These are owned fixtures on PG17,
not live Redshift/Aurora or old-server acceptance.

Receipt: .context/artifacts/popular-extensions/postgres-transport-verification.json.
All 284 native/planner/AWS tests across 47 signed targets pass with TZ=UTC;
strict Clippy2 passes. All owned fixtures stopped; no heavy job remains active.
Next prioritize remaining AWS originals/providers using .context/aws-next-control.md,
then final core/platform/package/PyPI/Homebrew qualification. Remaining transport
notes are in .context/postgres-auth-settings-next.md. The full all-five goal stays
active; no new public release or Homebrew Core acceptance is claimed.

## OAuth/debug checkpoint (November 15)

All 26 pinned extension settings now have runtime registrations, including
pg_oauth_token and pg_debug_show_queries. This does not claim complete libpq-option
or provider compatibility. OAuth uses the source's session-token/PGOAUTHTOKEN
precedence and acquisition-scoped clearing. The Rust transport implements supplied-
token OAUTHBEARER, validated issuer discovery, same-endpoint reconnect, rejection,
require_auth policy and channel-binding refusal. It does not load libpq OAuth
plugins or implement their interactive device flows. Token handlers are redacted;
the captured token uses zeroizing storage.

Query debugging writes the source's double-newline records to stderr (despite the
upstream setting description saying stdout). It is process-global even for a
session SET. Simple/extended queries, COPY and internal type lookups are observed;
pool maintenance is suppressed. The per-client observer is inactive for unrelated
driver clients, and retained statement SQL is omitted from Debug.

The unchanged attach_oauth_token.test passes on C++ and Rust against the owned
loopback OAuth fixture; all 23 paired supplemental scenarios pass, including no
credential transmission on policy/issuer failures. Debug (8 records), RDS (11), pool (16), secret (30)
and unified-scan (21) controls pass. The complete original PostgreSQL
inventory remains 145 PASS / 7 SKIP / 0 FAIL (postgres-upstream-auth-all-1); the
separate OAuth original supplies one of those prerequisites without rewriting SQL.
The other six original skips remain explicit. This is local protocol/disposable-
server evidence, not live OAuth/AWS acceptance.

Receipt: .context/artifacts/popular-extensions/postgres-auth-verification.json.
All 279 native tests across 45 signed targets pass with TZ=UTC; strict
Clippy2 passes. All owned fixtures stopped. The driver audit checks the registry archive hash, ten changed files, one added
Rust OAuth module and unchanged MIT/Apache licenses. Earlier failed exploratory
receipts remain. Final-source full core, vendor/version/snapshot behavior,
remaining authentication/provider/platform/package checks and PyPI/Homebrew
publication remain open. Next source notes: .context/postgres-auth-settings-next.md.
The all-five goal remains active; no new public release.

## Unified scan checkpoint (November 15)

Standalone postgres_scan/postgres_scan_pushdown and postgres_query now use the same
scan data and executor as attached tables. Standalone binding reads catalog type
OIDs/page metadata (or the SQL-standard metadata path) and retains independent
read-only repeatable-read sessions. It never resolves ATTACH's default secret.
COPY, CTID tasks, snapshot sharing, worker timeouts, LIMIT/OFFSET pushdown, physical
DML preparation and mixed-scan optimizer grouping are shared. postgres_query keeps
its parameter rules, use_transaction=false behavior, no CTID splitting, and the
upstream lack of filter pushdown/cardinality/explain callbacks. Legacy pushdown is
controlled by the function name, independently of the attached-table setting.

Standalone sessions close directly on early termination rather than draining COPY
while issuing ROLLBACK. Partial page chunks are combined to the standard vector
size; ordinary full/query/materialized chunks use the direct path. View mutations
now request CTIDs and report the server's missing-column error like upstream,
instead of failing later on an artificial NULL rowid. Query trailing whitespace/
semicolon handling follows StringUtil::RTrim's platform C-char behavior.

Receipt: .context/artifacts/popular-extensions/postgres-unified-verification.json.
The complete 152-file original PG inventory remains 145 PASS / 7 SKIP / 0 FAIL
(postgres-upstream-unified-all-1), with original SQL unchanged. All 21 new paired
controls pass (postgres-unified-control-2), including executed COPY/filter/limit/
snapshot evidence, tight-pool mixed joins, query parameters, optimizer-off execution,
prepared reexecution, timeouts and view diagnostics. Early-close controls pass on
both engines against a deliberately slow tail (postgres-unified-early-2).
Final legacy 26, attachment 27, mutation 22, DDL 16, parallel 12, pool 16, codec 39,
cache 13, secret 30 and interruption 6 cohorts pass (*-final-1). All 279 native/
planner tests across 45 signed targets pass with TZ=UTC; strict Clippy 2 passes.
All owned fixtures stopped; no heavy job remains active. Failed exploratory view
fixtures remain in views-1/2; final controls assert the server diagnostic specifically.

Next: of 26 pinned settings, pg_debug_show_queries and pg_oauth_token remain
unimplemented. Source notes are in .context/postgres-auth-settings-next.md. Complete
those actual behaviors, vendor/version/snapshot/error-path audit, remaining auth
and skipped-prerequisite supplements, then provider/platform/package qualification
and PyPI/Homebrew delivery. Keep the original four mysql-scanner guards and other
skips explicit. The last full core run is frozen 66389563; its ART regression was
fixed by e6504e3a and the targeted original passes, but final-source full core is
still required. The all-five goal remains active/incomplete; no new public release.

## Secrets/storage checkpoint (November 15)

Latest full original PG inventory: 145 PASS / 7 SKIP / 0 FAIL on Rust
(postgres-upstream-secrets-all-3), matching the C++ eligible-file count. The two
formerly failing secret files now pass unchanged on both engines. Four skips are
original mysql_scanner requirements; the other three require PGOAUTHTOKEN,
HAS_POSTGIS and AWS_RDS_DATABASE_AVAILABLE. Do not call the skips coverage.

The config provider accepts the pinned libpq option names/aliases, ordered alias
precedence, string conversion, redaction and ignored SCOPE behavior. ATTACH resolves
explicit/default secrets with ambiguity checks, preserves the original displayed
path, handles URI conflicts and applies attach-path overrides. Empty provider paths
are empty VARCHARs rather than NULL in duckdb_databases().

PostgreSQL secret storage probes before publishing ATTACH, creates a table only
when explicitly requested, preserves quoted/schema-qualified names, and reuses
registrations across detach/reattach. CRUD participates in the caller's transaction;
lookup uses scope/tie-break ordering and type-filtered SQL. The global manager lock
is released before remote I/O. Serialized-secret error text is redacted. Existing
DuckDB key/value serialization is reused; C++ writes/Rust reads and Rust writes/C++
reads both pass, including a write-back in the opposite engine.

RDS templates validate configuration without fetching credentials. Token generation
uses a separate connection/temporary secret/rollback lifecycle, existing Rust SigV4
signing, and the source's 840-second cache-age comparison. The signed token is used
as the PostgreSQL password on new pool connections. No-credential validation=none
returns the pinned unsigned URL. RDS name/password trimming follows the platform
C-char rule (including the observed non-ASCII suffix behavior), not Unicode trim.

Receipt: .context/artifacts/popular-extensions/postgres-secrets-verification.json.
All 30 paired secret controls pass (postgres-secret-control-3), including alias
collisions with 1..25 padding options, transactional rollback, storage conflicts,
scope/tie priority and URI handling. Both storage interop directions pass
(postgres-secret-interop-3). RDS controls validate SigV4 passwords and token reuse
across three connections per engine plus paired error diagnostics (postgres-rds-
control-3); this is an owned loopback protocol fixture, not live AWS validation.
The no-credential/provider SQL cohort passes both engines (postgres-rds-sql-control-3).
All 13 cache, 27 attachment, 22 mutation, 16 DDL, 12 parallel, 16 pool, 39 codec and
6 interruption regressions pass (postgres-secrets-*-1). All 284 native/planner/AWS
tests across 47 signed targets pass with TZ=UTC; strict Clippy 4 passes. Earlier
failed receipts remain. All owned fixtures stopped; no heavy job remains active.

PG fixtures now set SLT_FIXTURE_EXTENSION_DIRECTORY before either backend opens a
database. Pinned oracle artifacts are staged on the SSD, so original INSTALL
statements do not write extension binaries into the user's home. Ordinary corpus
defaults and original SQL remain unchanged.

Next: unify standalone postgres_scan[_pushdown] and postgres_query with attached
COPY/parallel/filter/limit/optimizer behavior (notes: .context/postgres-unified-scan-next.md).
Qualify remaining authentication/settings and skipped prerequisites, including the
AWS aws_secret_rds.test using an owned SSO fixture; its SQL only creates templates/
tokens, so no real RDS account is needed. Keep original wrong-require/mode-skip
files intact and use explicit supplements where appropriate. Finish remaining
provider/platform/package gates and PyPI/Homebrew delivery. The full core run of
66389563 had one ART export regression corrected in e6504e3a; targeted core secrets
and ART checks pass, but a full final-source core run is still required.
The all-five goal remains active/incomplete; no new release has been published.

## Catalog/cache checkpoint (November 15)

The PostgreSQL full original suite now reports 143 PASS / 2 FAIL / 7 SKIP on Rust
(postgres-upstream-cache-all-2), versus 145 PASS / 7 SKIP on the C++ oracle. Nine
prior failures moved to PASS, with no other status changes. The remaining failures
are attach_secret.test and attach_secret_storage.test. No original SQL was edited.

Schema lookup retains the implicit main schema even when USE selects public.
CREATE-time composite/enum columns lose their outer alias while preserving JSON,
matching PostgresTableEntry. The first postgres_execute query sends BEGIN and SQL
in one Simple Query message. Disallowed use_transaction=false and query failures
use the upstream Invalid Error classification and statement context.

The information_schema mode now uses SQL-standard discovery queries, maps the
pinned data_type names, returns no discovered enums/composites/indexes, and preserves
CREATE-time layouts until cache reload. Seeded zero-column tables are omitted like
upstream. pg_staleness_query_enabled is registered FALSE by the pinned source,
despite its description; explicit NULL selects the inverse of information_schema
mode. The paired default-setting control verifies that behavior.

Table signatures contain identity/name/revision with the source separators, sampled
through a separate pooled Simple Query connection. Custom placeholders, NULLs,
column-count validation, ignored extra columns and multi-statement last-result
semantics are implemented. Signatures are staged per transaction and promoted only
on commit; a generation guard prevents promotion into a cleared/replaced cache.
Settings changes clear attached caches. Table/view invalidation preserves other sets.

A supplementary lock test found normal catalog discovery was preparing SELECT *
over the user table and blocking external DDL in an open transaction. Discovery
now resolves catalog OIDs/types directly. The previously failing case passes on
both engines. The Rust driver patch still changes only client.rs/query.rs across
33 source files from tokio-postgres 0.7.18; both licenses are unchanged.

Receipt: .context/artifacts/popular-extensions/postgres-cache-verification.json.
All 13 paired catalog controls pass (postgres-cache-control-2), including executed
SQL evidence for the standard metadata path, type reloads, custom signatures and
rollback/cache-clear behavior. Final 27 attachment, 22 mutation, 16 DDL, 12 parallel,
16 pool, 39 codec and 6 interruption regressions pass (*-2 directories). All 279
native/planner tests across 45 signed targets pass with TZ=UTC; strict Clippy 5
passes. Initial failing receipts remain. All owned PostgreSQL fixtures stopped.

Next: postgres/RDS secrets and PostgreSQL secret storage; unified legacy/query scans,
remaining authentication/settings and skipped-prerequisite supplements; final
provider/platform/package gates and PyPI/Homebrew delivery. A fresh full core run
will be launched from this checkpoint under postgres-cache-conformance-1; inspect
its controller log/receipt and live process before starting another heavy job.
Until that run completes, the last qualified full core remains frozen 83d8da21.
The all-five goal remains active/incomplete; no new release has been published.

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

## SASL mechanism selection checkpoint (November 16)

When a server offered only `SCRAM-SHA-256-PLUS` over TLS and the client had
`channel_binding=disable`, the client answered with `SCRAM-SHA-256`, a mechanism
the server had not offered. It now follows libpq's `pg_SASL_init`: the PLUS
mechanism cannot be selected with channel binding disabled, plain SCRAM only when
offered, and otherwise the attempt fails with "none of the server's SASL
authentication mechanisms are supported". With `channel_binding=require` and no
PLUS mechanism offered the reason is libpq's "channel binding is required, but
server did not offer an authentication method that supports channel binding".

`scripts/postgres-tests/sasl_control.py` checks this against an owned protocol
fixture, because a real server always offers both mechanisms over TLS. Eleven
mechanism-list and `channel_binding` combinations produce the same selected
mechanism, GS2 header and failure reason on both engines.

The control also records two differences it does not gate on, and they apply to
every failed connection, not only to SASL:

- Wording. The reference reports `Unable to connect to Postgres at "<dsn>":
  connection to server at "<host>", port <n> failed: <reason>`, libpq's text.
  roastedDuck reports its own client's text, for example `error connecting to
  server: Connection refused (os error 61)` or `FATAL: <server message>`,
  without the connection string. Matching libpq's connection messages is open.
- Attempts. The reference opens two connections for a failed `ATTACH`;
  roastedDuck opens one.

Neither changes which connections succeed.

