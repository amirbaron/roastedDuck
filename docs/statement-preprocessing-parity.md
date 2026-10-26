# Statement preprocessing and import parity

Current component checkpoint: 2026-10-27. Source 5 passes 15 signed tests; the
shipping/public ownership refresh and Python verification remain pending.

The frozen C++ C API exposes real import statements and transaction handling. A one-table
export (CREATE TABLE plus COPY) produces four extracted statements, versus one on the
preserved Rust image. Chained BEGIN/import/COMMIT is six versus three; repeated imports
are eight versus two. Active-transaction imports also produce four versus one. Identical
export files, all errors/counts and actual loaded native-image identities are retained in
external `extension-overloads-20261026/import-extract-before-1-*`.

A separate paired Python before-fix control imports schema/load/CSV files from a registered
memory filesystem on C++, returning rows 1 and 2. The preserved signed main-context Rust
package fails by opening schema.sql locally. This package counterexample is scoped to that
before-fix image; it is not current-catalog Python acceptance. Evidence:
`import-preprocessing-20261027/registered-import-before-1-index.json`.

The working implementation now follows StatementPreprocessor: one transient transaction
for preprocessing, initial/chained transaction-state tracking, actual BEGIN/body/COMMIT
with all-errors invalidation and automatic rollback outside a transaction, and policy
SET/body/restore inside an active transaction. SELECT-bearing PIVOT bodies remain unwrapped.
The C API receives the actual expanded AST sequence, rather than an adjusted count.

Imports read schema/load files through the live local or registered filesystem, preserving
byte-valued paths, callback error types and native names. Read-only handles use explicit
size/read operations; COPY filenames are rebased to the requested directory. The same
reader serves direct statement execution. No file snapshot cache or table-block retention
was added.

TransactionInfo's invalidation and automatic-rollback flags now reach transaction execution.
Client binding/registration/appender operations invalidate a failed synthetic transaction
without taking over its query owner's rollback. Query execution handles automatic rollback
and cleans up rolled-back attachments. Closing an unread result still respects its explicit no-invalidation
path. The runtime policy setter/reset changes the active transaction; fresh startup policy
assignments reject C++'s exact context-required message. Validated config snapshots retain
the stored policy value separately from live transaction state. A signed C++ control copying
DBConfig options/user_settings proves that the value survives while a fresh BEGIN uses
standard error behavior (`cpp-policy-snapshot-verification.json`).

Workspace/all-target Clippy with warnings denied passes. The initial source-4 checkpoint passes 14 signed release tests. After the public ownership
control below exposed a prepare/query distinction, current source 5 passes all 15 cases:
six import/preprocessing and nine registered-storage tests. They cover extracted statement shape, successful COPY/commit, rollback of
created objects on an implicit bind failure, explicit-transaction abort versus ordinary
non-invalidating errors, startup/snapshot/reset lifetimes, and registered schema/load/CSV
files with a raw-byte table name. All 908 source hashes, executable hashes, case IDs and
strict signatures verify unchanged.

Evidence: external `import-preprocessing-20261027/{source-4/manifest.json,
sdk-verification-2.json,clippy-5-terminal.json}`. The current source snapshot is
`source-5/manifest.json`; source 4 remains the initial complete before-ownership-fix stage. Source 4 is the first complete overlay;
earlier draft overlays are explicitly excluded from release provenance. The new shipping
images are rebuilding. Public C API counts/execution, Python registered imports and policy
controls, broader SQL/shell/selected-client/ADBC/storage-WAL and coupled speed/memory gates
remain required. This component checkpoint does not complete the port goal.


The first signed public refresh now matches all five C API extracted counts/errors exactly
(4/4/6/4/8). Six of seven prepare/execute journeys match after mapping only the known
fixture-directory roots. The remaining implicit bind-error journey exposes a real ownership
difference: C++ leaves a failed extracted prepare in an aborted synthetic transaction;
the next query observes that abort and its query wrapper rolls it back. Rust rolls back
too early during preparation, so that next query succeeds. Execution-time failures and
active-transaction cases already agree. Preserve this distinction between client binding
and query execution before closing the gap. Evidence:
`import-preprocessing-20261027/public-import-comparison-1.json`; the source-4 shipping
library SHA is `47944c2d842c7c7174afb07bfb78cf14e5b79be336c112129f4f46b9ade35c12`.


The ownership correction now has a signed SDK regression that prepares/executes the
synthetic BEGIN and CREATE, fails preparing COPY, verifies that the transaction is still
active/aborted, then verifies that the owning query observes the abort and rolls back.
All 15 current tests and all 908 source hashes verify unchanged. Shipping native build 2
is running; the seven public C API journeys must refresh before closure is accepted.
