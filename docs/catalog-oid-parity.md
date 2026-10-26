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

The next extension-order implementation will preload only known statically linked
extensions explicitly required by startup options, then create the main database, then
load the remaining ordinary static extensions. Preserve explicit no-extension startup
and later LOAD. Use the existing idempotent loader and original extension order; normal
options are applied before storage initialization as today. Validate stored JSON values
and JSON-using defaults in both checkpoint/WAL reopen, read-only reopen, configured ICU
startup and disabled-autoload/explicit-load controls. A type/function needed during
storage/WAL loading must be available through its real autoload/config path; do not
silently ignore a failure or pre-load everything merely to mask order differences.


## Extension-order persistence control

The first signed SDK build with main-database attachment before ordinary static extension
loading passes all eleven persistence cases, including JSON defaults and TIMESTAMPTZ
checkpoint and committed-WAL reopening. Client settings pass five of six: the new disabled
extension assertion incorrectly expected no loaded entries, although built-in core_functions
remains available. The corrected assertion checks only optional JSON/ICU; production is
unchanged and its signed rerun remains pending. Evidence is preserved under
shell-final-gaps-20261025/oid-extension-sdk-verification-1.json. Native public OID/client
controls and final full-source acceptance are still pending.

## Remaining core-function startup boundary

A frozen-source audit separates153 built-in definitions (145scalar,8aggregate) from
367 core_functions definitions (300scalar,67aggregate), preserving generated list order.
The audit records names, kinds, source definitions and input hashes without reference OIDs:
shell-final-gaps-20261025/function-registration-origin-audit-1.json. Rust currently combines
both origins in builtin_scalar_functions/builtin_aggregate_functions before main attachment.
Moving only the six StaticExtension variants therefore cannot reproduce CPP's boundary.

The next stage must separate real core_functions registration from built-in registration,
retain aliases/overloads, and follow each frozen list's interleaved scalar/aggregate order.
Database-dependent built-ins (sequence/context/switch) must join the correct source list
without duplicate registration. Ordinary table/pragma/copy/collation registration and
extension placeholders still need their source lifecycle audited. Preserve disabled-loading
SDK behavior explicitly and verify stored expressions before changing the boundary.
This is a source audit and implementation plan, not a new numeric-parity claim.


The second signed settings run reaches the explicit-load timezone assertion: under TZ=UTC,
Rust reports UTC rather than the test's assumed Etc/UTC. A separate frozen CPP native
control also reports UTC with the same environment; evidence is
shell-final-gaps-20261025/oid-extension-cpp-timezone-1.json. The new test now sets UTC
explicitly after LOAD to avoid depending on a host default. Its third signed build/run
is pending; both earlier failed test logs remain preserved. No production timezone
behavior was changed.


The third signed client-settings run passes all6cases after those two test corrections.
Together with the first signed persistence binary's11passing cases, startup settings and
extension-backed file/WAL reopening now pass17SDK tests for this extension-order stage.
Receipt: shell-final-gaps-20261025/oid-extension-sdk-verification-3.json, linked to the
preserved persistence receipt. The native public-library rebuild is next; final full
corpus, client and coupled performance acceptance remain open.


## Static-extension boundary native checkpoint

Signed native oid-extension-library-1 SHA0d2b3909becdbdfcb82f21256e6909477cb06949f58fff082055702a33bab890
passes all652Go cases and30C API files/155cases/724646assertions. Every C API test's actual
loaded image/signature and source/library hashes are verified; workspace/all-target Clippy
passes with warnings denied. Together with17SDK tests, this verifies the static-extension
boundary stage. Receipt: shell-final-gaps-20261025/oid-extension-verification-1.json.

The maintained lifecycle script's schema query used schema_oid rather than duckdb_schemas.oid;
this verification-tool typo is corrected. New CPP/Rust controls have zero snapshot query
errors and retain all raw operation outcomes. Rust's three independent initial databases
all report21262(down from21857), whereas CPPremains20624. The numeric gap is still open;
core_functions registration and real placeholder/collation/dependency objects remain to port.
This staged evidence predates that next source change and final SQL/client/performance runs.


## Core-function source stage in progress

New function_lists.rs preserves all153built-in and367core definitions in frozen source
order, including interleaved scalar/aggregate entries. StartupFunctions consumes the
existing descriptors and rejects missing/duplicate/unclassified entries. The shared &&
set is split by actual GEOMETRY versus list argument types, then merged when core loads.
Built-ins now register after table/copy registration; core entries register after main
attachment, before the six ordinary static extensions. Existing SDK availability with
load_extensions=false is retained. Temporary registration descriptors are consumed at
startup; no table/result cache is added. The source-order audit matches all520definitions.

Workspace/all-target check passes after correcting an aggregate-name map key type. The
first signed native core-registration image opens successfully and all corrected lifecycle
snapshot queries succeed: Rust initial OID20530in all3instances, CPP20624. Numeric parity
is still open, including real extension placeholder/collation/dependency entry lifecycles.
Current native SHA6bed3404824057e3184653868992d314723b7ae8b334f5005a092253f6480d0f;
Go/C API, expanded persistence/settings and final acceptance are pending. Persistence
coverage now also includes core-function defaults, generated columns, expression indexes
and constraints across checkpoint/WAL reopen. No later-stage runtime success is implied.


The core-registration native image passes all652Go cases and30C API files/155cases/
752984assertions, with actual library paths and signatures verified. All four fresh
cross-engine scalar-storage journeys pass: each engine writes checkpointed and nonempty
WAL states, the other reopens/checks generated columns, defaults, expression index and
constraint failure/recovery, appends/checkpoints, then the writer reads the new row back.
Pre-replay files/WAL plus hashes remain in oid-core-registration-storage-1. Separate child
processes verify the actual native image; no mixed-engine fallback is used.

Workspace/all-target Clippy passes after doc-markdown-only corrections. The generated
lists are reproducible with scripts/generate-function-lists.py --check. SDK settings and
the expanded persistence test are rebuilding; no full SQL/client/performance acceptance
is claimed for this source stage yet.


## Current-source core-registration checkpoint

The final SDK build for this stage passes all17signed tests (6settings/11persistence),
including core-function generated/default/index/constraint behavior combined with JSON
and TIMESTAMPTZ across checkpoint and committed WAL reopening. Latest signed native
image2 SHA7b77ca405649127f421289e8122d69aec33e2eb7dcaffc98776a625fc47276e2
is rebuilt after doc-only source corrections and passes652Go cases with exact CPPIDs/
verdicts,30C API files/155cases/729050assertions, and all4cross-engine storage journeys
again. Source hashes, signatures and actual loaded images are verified. All2953function
name/type rows and per-kind counts match CPP; individual numeric OIDvalues still differ.
Receipt: shell-final-gaps-20261025/oid-core-registration-verification-2.json.

The remaining initial OIDdifference can now be localized without inventing allocations.
The first generated built-in !~~ is20312in CPPversus20222in Rust, a90-IDdifference before
that list. CPPsource declares42real extension-placeholder names (214overloads), and
3real built-in collations, each through ordinary catalog chain construction. Separately,
CPPCreateMainDatabase uses a real Connection: ClientData creates/initializes its temporary
catalog (identity plus main-schema chain), and the ordinary AttachedDatabase constructor
allocates a provisional entry identity before FinalizeAttach assigns its registered one.
Those lifecycles account for the additional4before the main database's visible identity.
Rust currently attaches directly and omits those object stages. Port the actual startup
connection/attachment flow and retain real provisional identities; do not create unused
objects or discarded counter increments just to reach a reference number. Later dependency
and extension/collation merge lifecycles also remain open. No final corpus/performance
acceptance is implied by this current-source staged checkpoint.


## Real collation entries in progress

The engine now creates nocase/noaccent/nfc as actual built-in catalog entries with
ordinary chain roots before the generated function list. They carry their own transform
functions and flags without separately publishing scalar functions. Collation metadata
now scans those entries rather than synthesizing the three names. Binding prefers the
registered collation's stored transform; standalone planner contexts retain their fallback.
Extension collations now have ordinary entry chains and IGNORE_ON_CONFLICT semantics for
both collation and serialization function, preserving an existing transform on repeat load.

Workspace/all-target checking passes and all18signed catalog tests pass, including actual
entry visibility, independent transform storage and duplicate-registration identity/flags.
The native rebuild and C++/Rust collation/SQL/client controls are pending. This changes
catalog/binding behavior; prior signed image2and its final-stage receipt remain preserved
as controls and do not prove this new source stage.


The signed collation native image matches all82paired public CPP outcomes exactly:
78successful queries and4expected errors, including column names/types and raw error
text. Cases cover inventory, sorting, DISTINCT, grouping, ordered aggregates, windows,
LIKE, combined/duplicate/invalid collations and default/reset behavior. A fresh signed
SQL runner passes all33frozen collation-related files, matching33CPPpasses with no
skips/failures. Input/runner hashes and signatures remain stable. Workspace/all-target
Clippy also passes. Initial database OIDis20536for3independent instances, versus CPP20624;
real built-in collation construction accounts for the changed allocation, with numeric
parity still open. Remaining Go/C API/catalog regression controls are running separately.
Evidence: collation-controls-comparison-1.json and collation-targeted-1/verification.json.


The real-collation image additionally passes all652Go cases with exact CPPIDs/verdicts,
30C API files/155cases/745284assertions, and the419-file catalog/transaction target:
413pass/6exactCPPskips/0failures. All18catalog tests,82paired public collation outcomes,
33frozen collation SQL files and workspace Clippy pass. Actual runtime image paths,
signatures and changed-source hashes remain verified. Receipt:
shell-final-gaps-20261025/oid-collation-verification-1.json. Final full-source client/corpus/
performance acceptance and remaining numeric lifecycle work are still open.

## Startup extension-setting controls

A new35-case native startup probe finds that the frozen source-build CPPbinary cannot
autoload ICU before its outer constructor: CMakeCache records autoload/autoinstall disabled,
and no ICU is installed in its normal extension directory. Its8new ICU-option cases
therefore fail, whereas Rust accepts them. Preserve those raw observations without treating
an unavailable reference extension as the intended available-extension contract.

The preserved official signed CPP1.5.6Python native image also exports duckdb.h and provides
an available-ICU control through those exact C API calls. It matches31/35Rust outcomes:
normal autoload succeeds on both, while4autoload=false cases must fail with the unrecognized
setting message and Rust currently accepts them. The official Python API independently
confirms valid startup values, autoload-disabled rejection and invalid TimeZone/Calendar
validation. The isolated local CPPICU extension copy failed strict signature validation
and was never loaded; its failure receipt is retained. No Rust fallback to a CPPengine is
introduced. Next fix startup flags, extension value callbacks and overwrite semantics using
real startup connection state; retain both reference build configurations explicitly.
Evidence: startup-extension-{cpp,rust,official-cpp}-1, startup-extension-official-comparison-1.json,
startup-python-reference-1.json and startup-extension-reference-1/signing-validation.json.


The expanded43-case startup C API cohort records actual image identities, settings
readback and raw errors. The official signed CPPPython1.5.6native image is used directly
through duckdb.h in a Python3.13process; Rust runs through the same interpreter/driver in
a separate process with only its selected native image. Before the fix,33/43outcomes
match:10Rust-only acceptance gaps cover disabled autoload, invalid TimeZone/Calendar and
invalid final overwrites. Both reference configurations and unmodified before-fix images
remain preserved; this does not change the frozen SQL/performance oracle.

The working source now uses a real startup connection/transaction for extension value
callbacks before main storage creation, honors autoload_known_extensions, and preserves
last-value/first-key semantics for repeated extension options. Pending options follow the
existing libc++case-insensitive map order. Workspace/all-target check passes after fixing
a test-only raw-message type comparison. A seventh SDK settings test covers disabled
loading, invalid values, overwritten values and no file creation on validation failure.
Native after-fix controls and the signed SDK run are pending. Evidence:
startup-settings-before-comparison-1.json and startup-extension-check-2.log.


## Startup settings and Parquet scope stage

The corrected startup native image2matches all43original available-ICU C API controls,
closing the10previous acceptance/value-validation gaps, including exact two-option error
order. The next6three-option permutations expose an additional Parquet-origin issue.
A two-connection public Parquet journey verifies45differences across85steps: automatic
SET leaks values globally, scope metadata remains GLOBAL after session overrides, and
invalid boolean input is accepted and corrupts the setting value. Both engines reject
SET LOCAL; the positive session control uses SET SESSION. Raw failed harness assumptions
and corrected before-fix records are retained, with no comparator normalization.

The working source now registers all5Parquet options as actual extension options when
Parquet loads, retaining their descriptions, boolean types/defaults and session-default
scope. They are excluded from core canonical lookup and fake built-in settings rows;
dynamic session/global values and reset semantics use the existing extension paths.
Extension value cast failures follow CPPValue::CastAs's InvalidInput wrapper. The public
settings table sorts by name after collecting core and dynamic entries, as CPPdoes.
C API flag enumeration already classified extension origins and remains unchanged.
An eighth SDK settings test covers actual session isolation, failed-cast preservation,
startup values and explicit LOAD registration. The public scope probe now also checks
actual Parquet BLOB/VARCHAR reads across both connections. Clippy passed before the final
settings-row ordering line; new native/runtime and final combined SDK/client/SQL checks
remain pending. Evidence: startup-settings-after-comparison-2.json,
parquet-settings-before-comparison-2.json and startup-map-common-terminal-3.json(32pass).


The migrated signed native image passes all49startup cases exactly, including all six
Parquet/ICU option-order permutations. All85two-connection Parquet steps now match the
frozen CPPnative reference exactly, including real BLOB/VARCHAR Parquet reads, session/
global/reset isolation, invalid-boolean errors and unchanged values after failure.
This closes the45observed setting-step differences; no case normalization or new waiver
was used. Evidence: startup-settings-parquet-comparison-1.json and
parquet-settings-after-comparison-3.json. The final combined SDK, broad C API/Go/SQL and
selected-client refreshes are still pending for this settings source stage.


## NULL settings and read failure control

The combined settings image passes all652Go cases and30C API files/155cases/705212assertions.
The expanded95-step NULL cohort then exposes5remaining differences: metadata changes SQL
NULL into an empty string, and reading Parquet with binary_as_string=NULL succeeds in Rust
while CPPthrows INTERNAL Calling GetValueInternal on a value that is NULL. The CPPprocess
remains reusable after RESET; raw errors, native traces and before-fix results are retained.

The working source now represents nullable settings metadata explicitly and renders a
VARCHAR NULL. Parquet scan/metadata option construction checks binary_as_string NULL at
its actual use and captures the genuine Rust origin of the corresponding internal error.
ErrorData::AddErrorLocation transfers a resolved stack_trace into the raw message before
the internal footer; the renderer is ported accordingly, preserving the direct-shell
trace/footer order. Signed common tests pass33/33, including trace placement/idempotence;
workspace/all-target Clippy passes before a test-assertion-strengthening change. The NULL
native rebuild and full combined SDK/client/SQL checks remain pending. Native frame bodies
are implementation/compiler-specific and are not replaced with CPPsymbols.


## Combined signed SDK checkpoint

The combined settings/persistence release test build finishes successfully and its two
immutable signed executables pass all19tests:8client-settings and11persistence. The new
settings cases verify invalid extension values, disabled autoload, last-value behavior,
no persistent-file creation on rejected configuration, Parquet session isolation, rejected
boolean casts without changing the value, explicit LOAD registration and actual SQLNULL
metadata. Existing checkpoint/WAL/torn-tail/index/generated/default/JSON/TIMESTAMPTZ cases
remain green. All15changed source/tooling hashes remain stable after this run. Evidence:
shell-final-gaps-20261025/parquet-null-sdk-verification-1.json.

The latest signed native NULL image also passes all652Go cases. Its95-step public Parquet
journey has90literal matches and5NULLread errors whose status/type, message, query context,
metadata and internal footer match exactly; only genuine native frame bodies differ.
The explicit frame-only checker rejects50negative controls and preserves literal mode's
5reported differences. Receipts: parquet-null-strict-frame-comparison-6.json and
parquet-null-literal-comparison-6.json. This is not a claim of literal native-trace identity.
Full C API, fresh shell/SQL and selected-client/ADBC/final performance refreshes remain
separate gates; the whole port goal is active.


## Fresh shell regression and actual extension order

The rebuilt signed shell passes392upstream cases with3shared skips. Its full768-case/
46PTY differential records1new unexpected difference plus2existing known differences:
json-errors-execution has the wrong setting candidate order after real Parquet settings
registration. The raw before-fix log and source overlay are preserved; this is a regression,
not a new waiver. All other differential cases retain their prior verdicts.

The frozen CMake-generated loader is authoritative: json,icu,tpch,tpcds,autocomplete,
core_functions,parquet. Rust's previous Parquet-first/ICU-last order and early core slot
were different. The source now follows that real registration sequence and puts core
registration between autocomplete and Parquet; it retains the existing core availability
for the SDK's load_extensions=false mode. No error text or candidate list is rewritten.
Source hash/control: extension-load-order-source-1.json. Checking, native and rebuilt shell/
SQL/client controls must verify the change before fresh corpus acceptance is transferred.


## Extension-order regression control

The corrected release build finished successfully. Immutable copies of the CLI, SQL
runner and native library are signed and strictly verified under signed-extension-order-1.
The targeted json-errors-execution comparison now matches C++ exactly, and the upstream
shell suite again passes 392 cases with the same three skips. The complete 768-case
comparison is still running; its interactive sessions must finish before acceptance.

The source overlay contains 900 modified/untracked files over the recorded Git HEAD,
with 899 build/source inputs and stable hashes. The before-fix snapshot and failing
comparison remain intact. No candidate text was sorted, rewritten or normalized.
Evidence: extension-order-images-1.json, extension-order-full-source-1/manifest.json,
shell-order-json-1/terminal.json and shell-upstream-order-1/terminal.json.


## Complete shell control and fresh corpus in progress

The corrected shell comparison finished with zero unexpected differences across all
768 cases, including 46 PTY sessions. The two prior known differences remain explicit:
implementation-specific native frames and opaque catalog ID values. The upstream suite
passes 392 tests with three shared skips. The new JSON candidate-order regression is
closed by the actual extension registration order, with no comparator waiver.

All 4,870 frozen SQL files and 1,522 data entries, including symlinks, verified against
the preserved fixture manifest before a new complete corpus run began. Source, archive,
signed-runner, reference and ratchet identities are recorded in full-corpus-order-1.
At the first progress audit, 1,787 files had completed: 1,736 pass, 38 skip and 13 fail.
One verdict differs from the prior complete run: geoparquet/no_spatial.test regresses
when restart rejects enable_geoparquet_conversion as an unrecognized startup option.
The run remains active and is not accepted as a passing full-corpus checkpoint.

The source-backed restart diagnosis is general: the Rust runner flattens session values
into startup options and restores no client state. C++ retains DBConfig's registered
extension option definitions/global values, then separately restores ClientConfig and
search paths. The fix must preserve those layers and retained option definitions without
weakening fresh-config autoload validation, modifying test expectations or skipping the
file. The frozen run and all before-fix worker errors will remain as evidence.


## Complete pre-fix corpus audit and restart implementation

The preserved pre-fix runner completed all 4,870 files: 4,779 pass, 17 fail and 74 skip,
with no crash or timeout. File coverage is exact and all skip reasons match C++. The
ratchet identifies exactly two regressions versus the previous complete run: GeoParquet
restart state and NULL temp-directory rendering. The other fifteen failures retain their
prior verdicts. Fixtures, signed runner and all 900 archived overlay entries verified
unchanged. The worktree advanced after that snapshot; this run is explicitly a pre-fix
control, not current-source acceptance. Evidence: full-corpus-order-1/terminal-summary.json,
fixture-after.json and ratchet.log.

The working source now has an opaque ConfigurationSnapshot, preserving existing Config
and C API cache equality behavior, and a separate ClientSessionSnapshot. Registered
extension metadata/global values initialize before pending startup options; client
settings, overrides, search paths and the query-log path restore independently. The
runner uses these APIs rather than replaying session values as unknown startup options.
The temp-directory getter and NULL setter now expose the disabled physical path, matching
C++ rather than changing NULL behavior globally.

A separate C API control confirms another related bug: after opening with threads=2 and
RESET threads, C++ returns 14 on the original, untouched and new connections; the pre-fix
Rust image returned 14,2,2. Explicit reset markers now preserve this distinction in global
state, read-only getters and configuration snapshots. New SDK regressions cover retained
option definitions without module loading, local/global isolation, search-path restoration,
callback lifetime, reset propagation and the disabled temp path. Workspace/all-target
checking and Clippy pass; the first Clippy run's doc-markdown issue was corrected. Signed
runtime and frozen SQL rechecks remain pending for this implementation.


The first signed restart SDK executable passes all four new regressions. Retained
extension definitions/global values survive independently from client overrides;
callback lifetime survives closure of the original instance without revalidation;
search paths restore; explicit resets reach untouched/new connections and configuration
snapshots; NULL temp_directory reports an empty disabled path. The fresh-config
negative control still rejects an extension startup option with autoload disabled.
Receipt: restart-sdk-terminal-1.json and restart-sdk-run-1.log. A new SQL runner/native
build is in progress for the two exact frozen failing files and public C API controls.


The new signed runner passes both exact frozen regression files unchanged. The broader
421-file cohort completes with 409 pass, two previously documented failures and ten exact
C++-matching skips; every verdict matches the earlier accepted complete run. This includes
all selected files containing restart directives plus settings, GeoParquet and temporary
storage cases. Changed-source and signed-runner hashes remain stable. The public C API
reset control now matches C++ on all three connections (14,14,14 after reset), versus the
preserved pre-fix 14,2,2. Evidence: restart-regressions-1, restart-related-1/verification.json,
and global-reset-rust-after-1.json. Fresh broad clients and a complete corpus run are still
required before transferring whole-source acceptance.


The corrected native library passes all 652 Go cases with exact C++ test IDs/verdicts,
30 C API files/155 cases/748,474 assertions, all 49 startup controls and all four current
cross-engine checkpoint/nonempty-WAL journeys. The 95-step nullable settings comparison
retains 90 exact steps and five differences limited by the explicit, strict native-frame
check; no other metadata/value/error differences are accepted. Source and image hashes
remain stable. Receipt: restart-state-verification-1.json. The shell is rebuilding, and a
new complete corpus plus final selected-client/ADBC/performance acceptance remain open.


The newly rebuilt signed shell passes all 392 upstream cases with the same three skips.
Its full differential/PTY refresh is running against the corrected core. Source fixes
are validated by the focused and broad restart cohorts, but fresh full-corpus acceptance
is still required; the preserved 4,779-pass pre-fix run is not substituted for it.


## Complete restart shell verification and new full-corpus run

The corrected signed shell completes all 768 differential cases, including 46 PTY
sessions, with zero unexpected differences. The two existing exceptions remain native
frame bodies and opaque catalog identities. All 392 upstream shell tests pass with the
same three skips. Evidence: `shell-diff-restart-1/summary.json` and
`shell-upstream-restart-1/terminal.json` under the external task directory.

A new complete 4,870-file corpus run is active against the immutable signed restart
runner. Before launch, every frozen SQL file and all 1,522 data entries verified,
including symlinks. The source overlay records 900 build inputs; the command records
normal engine threads and one worker. This run has no acceptance verdict yet. Its
complete outcomes, ratchet and post-run source/image/fixture checks must finish before
it replaces the preserved pre-fix control. Evidence: `full-corpus-restart-1/manifest.json`
and `restart-fixture-before-1.json`. Selected-client, ADBC and coupled performance
acceptance remain open; no table-cache retention was added.


## Complete signed restart corpus acceptance (2026-10-26)

The new runner completes all 4,870 frozen files: **4,781 pass, 15 existing documented
failures, 74 exact C++ skips, zero crashes and zero timeouts**. Both pre-fix regressions
pass, and every file verdict matches the prior accepted complete run. The ratchet
passes with 4,855 listed entries and zero regressions. Exact SQL/data file sets, all
1,522 fixture data entries, the signed runner, reference, ratchet, 900 build-input hashes
and all 901 archived overlay entries verify unchanged. The source overlay's Git base
predates the subsequent documentation-only commit; build/source hashes remain stable.

This accepts the current signed restart SQL checkpoint, not whole-port completion.
Catalog identity, broader selected-client/backend contracts, final ADBC/release refresh
and coupled speed/memory acceptance remain open. No new exception or cache was added.
Evidence: external `full-corpus-restart-1/{terminal-summary.json,fixture-after.json,ratchet.log}`.


## Main database filesystem context counterexample (2026-10-26)

A paired public Python control on the current signed source-9 wheel exposes two core
startup gaps. After opening an existing local main database, C++ rejects the first
WAL commit after disabling LocalFileSystem and rolls back the inserted row. Rust commits
it. The same attached-database control matches exactly after the retained-backend policy
fix; disabling general external access still permits main-database writes on both.
Separately, startup configuration disabled_filesystems is rejected by C++ with
"Cannot change/set disabled_filesystems before the database is started", but Rust
accepts it and writes. All raw rows, errors and file hashes are retained in external
`registered-storage-20261026/main-policy-comparison-4.json` and the paired reports.
These are open functional gaps, not accepted exceptions.

The frozen source explains the missing context. DatabaseInstance::CreateMainDatabase
constructs a real Connection, begins a transaction, calls AttachDatabase through its
ClientContext, then commits. Rust still attaches main directly outside a connection's
file-opener scope. Consequently its main block manager captures no live disabled-system
registry. This is also the previously identified real startup connection lifecycle
whose temporary catalog/schema constructors consume three instance-owned OIDs. Port
that actual context and transaction flow; do not add unused objects or numeric offsets.

The disabled-filesystem setter calls GetDB and rejects a null instance before changing
state. Startup validation must match that behavior across configuration entry points.
Audit related instance-dependent setters and snapshot semantics: C++ restart copies
DBConfig options/user_settings separately from client state, so runtime registry state
must not be blindly flattened into startup parameters. Preserve runtime SET behavior,
configuration equality/C API caching, signed regressions and current source provenance.
Current Python matrices are verified for the signed before-fix checkpoint; broader final
release/corpus/client/performance acceptance remains open.


## Real main startup client and startup-setting layers (2026-10-26)

The working implementation now creates the actual startup connection, begins its
transaction, uses its file-opener scope for main type detection/attachment and default
database assignment, and commits before releasing that client. Ordinary static extensions
still load afterward in the frozen order. This supplies the main block manager with the
live disabled-filesystem registry and consumes only the real temporary catalog/schema
identities. No allocator offset or discarded placeholder object was introduced.

Paired startup controls establish nine instance-dependent setters that C++ rejects before
startup: disabled_filesystems, enable_logging, logging_mode, logging_level, logging_storage,
enabled_log_types, disabled_log_types, allowed_paths and allowed_directories. Fresh Rust
configuration setters and direct Config option vectors now reject them with the matching
message. Config structure/equality and C API cache keys remain unchanged.

A separately compiled, signed C++ control copies exactly DBConfig options/user_settings,
as RestartCommand does. It proves that logging and disabled-filesystem managers reset,
while path allowlists persist and still permit the allowed CSV after external access is
disabled. Rust's opaque configuration snapshot now retains those validated database
options separately from fresh setter assignments; instance manager state is not replayed
as startup configuration. Client session restoration remains a separate layer.

The initial main path, its WAL/checkpoint/recovery paths and its configured temporary
directory are explicitly allowlisted when startup external access is false, matching
Configure. This preserves main database and main CSV access without granting general
external access. The disabled_filesystems getter exposes C++'s empty value while registry
policy remains enforced independently.

Workspace/all-target Clippy passes. Four signed focused SDK regressions pass, covering
main-file rejection/rollback, nine runtime-only startup settings through setter/direct
configuration, mixed snapshot lifetimes with session restoration, and existing/new main
DB plus CSV access with external access disabled. Source and signed-image hashes remain
stable. The broader settings/persistence/registered-storage/restart SDK build is active;
public Python/C API/Go controls and current numeric OID observations still require refresh.
Evidence: external `main-context-20261026/{sdk-verification-1.json,source-1/manifest.json,
cpp-snapshot-state-terminal.json,startup-runtime-before-index.json}`. The whole port goal
and final signed release/corpus/client/performance gates remain open.


## Broader signed main-context SDK checkpoint (2026-10-26)

All four rebuilt SDK targets pass on immutable signed executables: eight client-settings,
eleven persistence, eight registered-storage and four restart tests. Together with the
four focused main-context tests, these are 35 passing current-source tests, with no ignored
cases in the batch. Every selected case ID is inventoried; all 903 build-input hashes,
executable hashes and strict signatures verify unchanged. This covers existing configured
extension callbacks, snapshot lifetimes, typed settings and checkpoint/WAL behavior after
the startup-client/configuration change.

Evidence: external `main-context-20261026/sdk-verification-{1,2}.json`, per-target logs and
`source-1/manifest.json`. The fresh shipping C API library, SQL runner and shell are building
without the ADBC test feature. Public C ABI/Go/Python and targeted SQL controls must still
verify the shared change before wider acceptance transfers. A 589-file frozen selector
covers restart directives plus settings, logging, attachment, transaction, GeoParquet and
temporary-directory files; it is prepared, not executed or substituted for the final full
corpus. Final release, selected clients, storage/WAL and coupled performance gates remain open.
