# Catalog OID lifecycle parity

The public C API controls in shell-final-gaps-20261025/oids-{cpp,rust}-1 create two
independent in-memory database instances in one process, inspect them, create/fail/no-op/
rename/comment/drop-create objects in one, inspect the untouched instance, close both,
and open a third. Runtime function-image paths and native image hashes are recorded.

CPPinitial database OID is20624in each instance for this frozen build. Rust advances
21187,22378and23659because CatalogEntry::next_oid uses one process-global atomic.
Rename/comment preserve table IDs on both. Bootstrap and later allocation order differ
as well; fixing instance ownership alone cannot claim numeric parity. These are actual
observations, not permission to hardcode a display value or reset a global counter while
other instances live.

Frozen CPPDatabaseManager owns next_oid initialized20000. CatalogEntry constructors
obtain IDs from their catalog's DatabaseManager, and attached database registration
consumes the same allocator. Introduce instance-owned allocation shared by system,
attached and temporary catalogs; preserve standalone catalog/test construction and MVCC
entry-copy identity semantics. Trace constructors, defaults/lazy builtins, conflict paths,
merge/copy allocation and attach/load order before claiming full numeric parity.

Use public controls and independent instances to prove allocation isolation, uniqueness,
object identity through metadata changes, failure/no-op allocation behavior and restart/
attach persistence. Run appropriate signed catalog/engine/client/corpus checks after a
shared change. CPPbuild-specific bootstrap ordering must be matched by real construction
behavior, not by a magic offset, display rewrite or cached reference results. Other shell
and client/performance acceptance gates remain open.

## Instance-owned allocator implementation in progress

The first wiring pass compiles across the workspace/all targets. After removing two
unused imports, signed catalog tests pass11/11, including independent allocator isolation,
shared attached/temp allocation and lazy default-type allocation. A follow-up source audit
also routes metadata-copy constructors and lazy coordinate-system IDs through the owning
instance. Standalone entry/catalog constructors retain compatible fallback construction;
production instance paths use explicit catalog/allocator entry methods. Native C API build
is active; public lifecycle results and numeric parity are not yet accepted.

This pass deliberately retains bootstrap/attachment order and conflict behavior so the
next public probe can separate instance ownership from remaining ordering differences.
The existing complete SQL/client acceptance remains on its recorded pre-OID images.
A shared allocation change requires fresh verification after the implementation stabilizes.


## Public isolation checkpoint

New signed oid-library-1 SHA91598e872d6f5aa17d787c5de32474df8d234b88cbbc5101dcdd45860814f607
passes the identical public multi-instance probe: Rust's first, second concurrent and
third restarted instances all begin21187instead of drifting21187/22378/23659. CPPbegins
20624for all three. Instance isolation is fixed; full numeric parity remains open.
Receipt: oid-isolation-checkpoint-1.json and oids-rust-2 raw lifecycle observations.

The richer inventory matches public function counts but shows different ID order.
Frozen CPPCatalogSet::StartChain constructs an INVALIDplaceholder that consumes an OID;
rename/drop also construct tombstones. Rust's version placeholders currently do not.
CPPdefault schemas/views/macros are lazy; Rust eagerly constructs its defaults. CPPalso
assigns the attached database's OID in FinalizeAttach after initialization, whereas Rust
currently assigns before main-schema construction. Match these real lifecycle rules in
subsequent stages; do not compensate by counting a constant number of missing objects.

Inventory probe1used schema_oid instead of the actual duckdb_schemas.oid column; its
error/raw results are preserved. Corrected probe2has no query error in its first inventory
stage and records exact schemas/function/view/type IDs. Full Go/C API/catalog/Clippy
checks are needed for the current ownership pass before layering ordering changes.


## Ownership pass verification

The ownership image passes all652pinned Go root+Arrow cases and all30C API files:
155cases/753956assertions, with actual runtime images and unchanged native hashes verified.
Workspace release all-target Clippy passes with warnings denied. The earlier11catalog
tests cover the initial wiring; metadata-copy/CRS follow-up is additionally compiled and
exercised by the client/C API runs. Receipt: oid-ownership-verification-1.json.
This is a stable isolation checkpoint, not numeric parity or final full corpus acceptance.

Next implement OID consumption for actual chain placeholders/tombstones as specified by
frozen CatalogSet::StartChain/DropEntryInternal/RenameEntryInternal. Every new entry chain
and actual tombstone should consume from its owning allocator; failed/no-op operations
must follow the same source lifecycle. Preserve MVCC visibility/rollback and copied entry
identity. Use the existing placeholders as real internal catalog objects/versions, not
an arbitrary offset. Lazy default creation and initialization order remain later stages.

## Next lazy-default stage design

Public inventories currently have the same names/counts, so do not remove functions or
views to reduce the counter. Default schemas must be created on lookup/scan; schema sets
need default view/macro factories in the frozen definition order. CatalogSet lookups,
scans and suggestion lookups must materialize the applicable defaults, as CPPdoes, while
committed generated defaults avoid normal-chain placeholders. Factories must not capture
an owning Catalog Arc and create reference cycles. They must not hold a catalog lock
while parsing/binding a view that can reenter catalog lookup. Preserve one published
entry under concurrent lookup and default immunity to rollback.

Install generators without eager entry construction/OID consumption. Deferred schema
creation must install its own view/macro generators too. A generic factory interface can
carry the catalog name/allocator and frozen definition data; core catalog code must not
depend on the database/planner crate. Keep error behavior explicit instead of silently
ignoring factory errors. This design must be validated with existing view/macro/catalog
suites, public inventories, concurrent default creation and client/corpus checks before
claiming bootstrap parity. Placeholder/tombstone IDs and attach/extension order still
need their own source-driven implementation and controls.

The first bounded lazy stage will retain eager parsing of frozen default definitions,
which already fails startup explicitly on malformed SQL, but defer actual CatalogEntry
construction and OID allocation. Factories then only clone validated definition data
and create an entry, so existing infallible catalog lookup APIs do not swallow errors.
Parsing under catalog locks is avoided, and no owning Catalog Arc is captured. This
matches observable entry allocation laziness without introducing a fallible factory into
an Option-returning lookup. Full parse-time deferral is not claimed by this stage.

The initial deferred-entry implementation passes workspace/all-target compilation and
13signed catalog tests, including first-lookup OID allocation, one published entry under
concurrent lookup and release of the default registry after catalog teardown. A new
native image is building for public inventories and complete client checks; this is not
runtime/catalog-wide acceptance yet. Creation/conflict paths must also trigger defaults
before starting a new chain, matching CPPStartChain, so callers cannot shadow a default
merely because it has not been looked up. Keep that audit explicit in the next controls.

## First lazy-entry native checkpoint

Signed oid-lazy-library-1 SHAee293309d5058d1802abf7b2e0e8e4a73284395e836000562d3377aba501babb
passes the complete652-case Go root+Arrow suite. Public inventory queries have no errors;
function names/counts,47internal views and244internal type rows match CPP. Initial Rust
DBOID becomes21020instead of21187through actual deferred construction; it still differs
from CPP20624. This is progress, not numeric parity acceptance.

The transactional create path now triggers generated defaults before starting a chain,
matching CPPStartChain. A new test checks that an unobserved internal macro cannot be
shadowed and remains the same committed object after rollback. Its next catalog build
is active; the first native checkpoint predates this small conflict follow-up. C API,
Clippy, broader catalog/default suites and full core verification still follow.


## Lazy-entry follow-up checkpoint

The follow-up passes14signed catalog tests, workspace/all-target release Clippy with
warnings denied, all30C API files (155cases/795450assertions) and all652Go cases on the
same signed oid-lazy-library-2 image. Runtime paths and hash receipts are retained. Its
SHA is d631be95705f0d15ddeb9c076c6c73374fb07e30f73f6541f33a0a4e8c1b4a27.
The create/default conflict path is included; raw first-image results remain separate.
Receipt: oid-lazy-verification-1.json. A fresh Slt runner is building for frozen macro/
schema/catalog-introspection coverage. Numeric bootstrap and full corpus acceptance are
still open; no reference display value or allocation offset is hardcoded.


## Frozen targeted SQL verification

A freshly built/signed current runner passes169catalog/pg_catalog/storage-introspection
files. An additional54macro/schema/SQLite-relevant files, selected separately to close
selector coverage gaps, produce48passes and6exactCPPskips. The223unique files have no
failures and every verdict/skip reason matches CPP. Both outputs remain separate and
immutable; no earlier results are merged. Runner hash/signature stays unchanged.
Receipt: oid-lazy-targeted-verification-1.json and lazy-targeted-{1,2} logs/manifests.

This validates targeted default/catalog behavior on the current lazy-entry implementation;
it is not a new4870-file result or numeric OID parity. The remaining allocation-order
stages and a fresh full corpus after stabilization remain required.

The next bounded placeholder step starts with transaction-created chain roots and delete
versions. Each such real Version will retain its allocated internal node OID, mirroring
the identity of CPPINVALID/DELETED entries, while keeping its public entry optional for
MVCC visibility. Allocate delete IDs only after existence/conflict/internal-drop checks
succeed. Failed/no-op drops must not consume a tombstone ID. Commit/rollback behavior
must remain unchanged. Rename chains, ordinary builtin registration versus default committed
creation, and dependency entries require separate verification; do not generalize a
fixed per-operation increment beyond the actual node constructors.

The initial root/delete-node step passes15signed catalog tests and workspace all-target
release Clippy with warnings denied. The new lifecycle test verifies allocation only for
actual chain/delete versions and entry identity after rollback; no native numeric parity
claim yet. Rename remains separate: CPPconstructs an old-name RENAMED node, its DELETED
node, a new-name RENAMED node, and an INVALID root only when the new chain is absent,
then installs the identity-preserving altered entry. Represent these actual versions and
register undo/cleanup in the same order; do not emulate them with an unexplained counter
increment. Conflicts must be checked before destructive changes, and rollback must restore
old names/visibility without leaking a new name.


The rename-node implementation passes16signed catalog tests, including old-reader
visibility, identity-preserving commit and rollback, and the actual internal node sequence.
Workspace/all-target Clippy passes with warnings denied after replacing a panic-style map
lookup with entry access. A fresh C API image is building for public lifecycle/client
verification. These native checks and full numeric/bootstrap parity are not yet accepted.

## Node native checkpoint

Signed oid-node-library-1 SHA65c62138993ad0ad27e8e5c3887cbb4af1fa6a26c3c1d3b393cc85579a73a8f2
preserves public table identity across rename/comment and independent instance allocation;
all652Go cases pass. Repeated introspection versus no repeated introspection produces the
same next marker-table ID on each engine, so repeated read-only catalog snapshots do not
cause unbounded drift. Controls oid-repeat-{cpp,rust}-{0,1} preserve the raw observations.
Numeric IDs still differ. CPPdependency-manager entries also own catalog-chain identities,
where Rust currently stores logical dependency lists; those real lifecycle objects must
be audited before claiming later user-object sequence parity. Do not replace them with
fixed per-dependency increments.


## Node targeted SQL acceptance

The current signed node runner completes419frozen files:413passes/6exactCPPskips/0failures.
All verdicts match both CPPand the prior complete baseline, including70transaction files
and relevant ALTER/rename/drop/dependency/default/catalog tests. No older outputs are
merged. Runner signatures/hashes stay unchanged; raw logs/worker records and current
source snapshot are preserved. Receipt: oid-node-targeted-verification-1.json.
This accepts the targeted version-node behavior, not the full4870corpus or numeric OID
bootstrap parity. Builtin creation, default committed creation, dependency entries and
startup/attach order remain separately audited work.

Ordinary builtin initialization needs a distinct committed-chain insertion API: publish
its named entry plus the actual allocated INVALIDroot without a user transaction undo
record. Generated defaults/checkpoint-restored committed entries retain direct insertion.
Rust's Transaction::system is a lookup view, not CPP's bootstrap CatalogTransaction, so
do not force bootstrap writes through it or leave uncommitted timestamps/undo retention.
Verify named entry IDs and the next allocated ID through the chain API, including that
an existing entry does not create a second root. Apply only at source-confirmed ordinary
builtin/main-schema construction sites; merges/defaults/load paths have separate semantics.

The committed ordinary-builtin-chain step passes16signed catalog tests. Main-schema
and regular register_builtin now construct real INVALIDroots; generated default/direct
committed insertion remains distinct. Public numeric parity is still unverified for this
new stage. Next separate attachment identity timing: construct/initialize the attached
catalog and its entries, then allocate its database OID at final registration as CPP
FinalizeAttach does. Standalone Catalog::new behavior stays explicit for tests/users;
the database manager can use an initially unregistered catalog. Do not move extension
loading simultaneously: storage/WAL/option-driven extension needs require their own
controls and lifecycle port.

Ordinary builtin chains and one-time attachment identity finalization now pass17signed
catalog tests. Database-manager attachment uses an unregistered catalog while storage/
entries initialize, then finalizes its OnceLock-held identity before publication. Repeated
or concurrent finalization consumes no additional IDs. Standalone catalog construction
retains its explicit immediate-registration behavior. Workspace check passes; current
Clippy is running, and public/native/client/storage/corpus refresh is still pending for
these latest startup changes. Extension-loading and full builtin registration order remain
unchanged and open; no numeric parity claim is made from the unit tests.

The signed startup image (oid-startup-library-1,
SHA77fef9dd3a8292f88ceb6db89c8ecf4c83d44721c1319580295540ea2a03f98a) completes the public
lifecycle probe with instance isolation intact. Initial Rust database OID becomes21857:
real ordinary catalog-chain nodes now consume IDs, so a larger numeric difference alone
is not a regression in lifecycle fidelity. Numeric parity remains open. Extension-loading
and function-registration order are unchanged; validate native clients/storage before
changing those next. No hardcoded offset or display correction is used.


The ordinary-builtin-chain/attachment-finalization native image passes all652Go cases
and all30C API files/155cases, with runtime-image and signature checks. Numeric startup
parity remains open at Rust21857versus CPP20624. Extension-loading order is the next
separate lifecycle step. Configured extension options must load their required extension
before attachment; ordinary remaining statically linked extensions should load after main
database creation as CPPDuckDB construction does. Stored JSON/ICU types, WAL replay,
read-only/reopen/data-file attachment and disabled-autoload cases require controls before
accepting that move. Do not indiscriminately disable early option/type dependencies.
