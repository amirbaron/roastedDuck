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
