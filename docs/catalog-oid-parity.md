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
