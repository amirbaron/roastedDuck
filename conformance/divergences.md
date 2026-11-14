# Conformance divergences and open defects

Corrected candidate status (2026-11-03): source2078 fixes an index-publication race that could omit committed rows from indexed results. The old Linux ARM artifact failed the unchanged concurrent-index test in14/30 replays; corrected signed2080 passes30/30, with3/3 C++ controls. A controlled storage regression fails on old code and passes after the fix; all10 storage constraint/index tests and strict checks pass. Corrected macOS2088 passes the six focused control groups (1,893 records per engine) and110,464 CAST/TRY_CAST queries per engine. Fresh full SQL and all remaining release gates are still pending. Source1959/core1961/Linux2005 are retired from final acceptance; older results below retain their historical scope. The fix adds no cache or query-memory budget. Physical iOS execution remains unverified because no device is available; it is not waived.

Every raw SQL verdict is retained. The documented applicability boundaries below
cover private C++ extension ABI tests and tests whose expected OOM does not occur
under Rust's lower-memory execution. Stable C ABI extension loading remains a
required feature. Open defects and unverified release gates are not exclusions.
Third-party native crypto is allowed; the DuckDB engine must remain Rust.

Current correctedsource1337/release1344 fullRust1355 is complete:4826pass/30skip/
14documenteddivergences, zero crash/timeout/resourcekills. The completed exactC++934
baseline1302 has4839pass/30skip/1knownreferencefailure. All30skip IDs/reasons match;
the only change from Rust1292 is spillcleanup fail->pass. All rawverdicts are
retained in review1356. Its reviewedgate permits finalnative/clientverification
only; it does not complete required unit/client/platform/storage/performance gates.

## Hash-join spill cleanup: corrected current-source corpus

Current full1302 source1292/signed1295 fails the original
`test/sql/outofcore/leftover_temp_files_issue_6420.test_slow` at its final
`DROP TABLE ans` check: `duckdb_temporary_files()` remains nonempty. C++ passes.
This is a functional defect, not a lower-memory or extension-ABI exclusion.

Live source now explicitly releases operator and pipeline source states after
tasks drain, even when a completed worker briefly retains shared task metadata.
The deterministic regression holds that metadata alive while checking data
release. Before1336 preserves the old implementation with only the new test;
after1337 changes exactly two production files. Unit proof1352 now verifies actual fresh compiler success, strict signing and
pinned authentication for old-fail/new-pass, plus all16 execution-library tests.
The first after1338 control reused a stale before executable and is preserved as
invalid evidence; dedicated fresh target1346 supplies the valid corrected result.
The original unmodified SQL case now passes three Rust runs on signed1344; the
five original paired lifetime cases and CAPI155 also pass (audit1357). Full1292
retains its raw failure; all4870 correctedRust originals now complete in1355. Its reviewedgate permits
final unit/client/platform/storage/performance verification; these remain open.

## Temporary-directory defect: verified component, final sweep pending

`test/sql/tpch/dbgen_error.test` is not a lower-memory waiver. The temporary-file
manager used recursive directory creation, whereas C++ `LocalFileSystem::CreateDirectory`
creates one directory and fails when its parent is missing. The old Rust run left
`.unrecognized_folder` behind; its birth time matches that run, and the shared root
then changed the conditions for a later C++ run of the test.

Frozen source226 now creates only the requested directory, requests Unix0755
subject to umask, and formats OS errors/errno and existing-file errors like the
observed C++ controls. Signed before185 fails the missing-parent regression;
fresh signed227 passes21 unit and11 integration tests under umask000, Clippy and
formatting. Public before222 confirms the original parent/mode/error differences.
Fresh signed native228 passes all155 C API cases. Exact public229 matches five paired filesystem scenarios (ten executions), including raw errors/errno and umask000/022/077 modes. Original
unchanged SQL230 passes both engines in fresh separate roots without creating
parents. The full corrected-artifact SQL260 collection now passes this case on
both fresh roots336, with all original fixture/native checks stable342. Remaining
core/client/platform/performance release gates remain open.
Original failures and the polluted root are preserved; full verification must use
fresh separate engine roots and unchanged SQL/data inputs.

Evidence: external `platform-matrix-20261029/` observations181–184, source185/186,
fresh fixtures191, before188/222, source226 and unit227. Failed190 formatting and
219 cached-old-binary evidence remain unmodified; clean221/227 replace those as
unit proof. Original SQL and the conformance passing list have not been changed;
independent unit/client regression coverage has been added.

## Leaner than C++

These tests assert that DuckDB runs out of memory or spills to disk at a given limit.
rustdb fits the same work into less memory, so the expected error or spill never
happens. Preserve these lower-memory behaviors; final performance acceptance follows
`docs/performance-acceptance.md`, with coupled speed/memory measurements and
individual query outliers. Exact independent speed, memory or spill parity is not
a requirement.

| test | asserts | rustdb |
|---|---|---|
| `test/issues/internal/test_5457.test` | `EXPLAIN SELECT 42` fails under `memory_limit='300KiB'` | explains it within the limit |
| `test/sql/settings/enable_caching_operators.test_slow` | a hash-join probe over 8 MB strings with caching operators enabled runs out of memory under `memory_limit='300mb'` | streams the strings through within the limit |
| `test/sql/json/issues/read_json_memory_usage.test` | `read_json_objects` with 8 threads runs out of memory under `memory_limit='50MiB'` | reads the file within the limit |

## C ABI extension bridge: implemented components, final verification pending

Layout/signature checkpoint430 adds ten independent C layout compilations across
all five required target slices for the404-field v1.2.0 prefix and546-field full
table. Official core-signed inet bytes are accepted and a one-bit mutation rejected
with unsigned/community permissions disabled. Rust's data-only test explicitly
allows platform metadata mismatch to validate a Linux file on macOS; no downloaded
code ran. These do not close signed-C-ABI runtime or final platform/client gates.

Shared-library checkpoint420 adds actual public C API INSTALL/LOAD control402
and exact metadata-error408/initializer-error418 comparisons with C++ oracles.
It fixes validated string-option boolean handling, required .info mode fields and
initializer error categories. Workspace/all-target strict Clippy419 passes. These
remain development components; broad signed/version/lifetime/ABI/platform/client/
release/performance acceptance is still open.

Implementation checkpoint: the live bridge now passes native macOS SDK control360
for explicit unsigned loading (scalar/NULL/vector/repeated LOAD/catalog/reconnect,
two independent databases) and rejects the fixture with unsigned loading disabled.
Signed native unit355 passes independent OpenSSL signature/tampering/trust and
version checks; workspace/all-target strict Clippy361 passes. These are development
components. Initial local INSTALL now also passes signed native379 and exact C++
.info bytes373; latest workspace Clippy382 passes. Initial force/origin/gzip/permission-mask controls now pass385/391, matching file
modes0666 filtered by umask and directory0755 filtered by umask393; workspace
Clippy392 passes. Broader metadata/signature/lifetime controls and
shared-library/selected-client/platform refreshes,
complete metadata/signature/lifetime controls and final common-source release/
performance gates remain open. Source226 receipts remain before-image evidence.


The signed pure-C control341 passes on C++1 and fails on Rust228. It targets the
stable C_STRUCT API v1.2.0 using the original public extension header, with no
C++ engine symbols or linked-engine dependency. C++ loads the callback and returns
12/7/0/NULL, sums twenty vector inputs to330, permits repeated LOAD and reports
the extension as loaded. Rust returns Not implemented while loading the file and
the function remains absent. This is distinct from the C++ binary ABI exclusion
below and is not an accepted exception.

The synthetic fixture explicitly enables unsigned extensions; project content
signatures authenticate its compiled payload before loading and authenticate both
selected engines and the plugin again afterward. All actual native images match.
The source audit325 finds all546 extension-table function names already among the
547 shipping exports, but that alone does not provide the missing loader/access
bridge or prove C struct layout. Uncompiled design333 records dependency,
context ownership, signature, catalog, lifetime and version requirements.

The first control322 failed compilation323 before loading because its callback
needed an earlier DUCKDB_EXTENSION_EXTERN declaration. Corrected337 adds only that
declaration; original failures remain. Bounded control340/pipeline341 then compile,
sign and execute successfully as an oracle comparison. It confirms a defect,
not parity completion. No reference failure occurred in successful341.

Further source226 client/platform builds were deferred321/343 before execution.
Implement the C-ABI path, freeze a fresh integrated candidate and rebase required
final gates, retaining default signature enforcement and the lower-memory policy.
Evidence: external `platform-matrix-20261029/{capi-extension-control-pipeline-341/,
full-corpus-and-cabi-gap-audit-342.json,old-client-queue-deferred-343.json}`.

## Private C++ extension ABI boundary

The suffix `.duckdb_extension` alone does not identify an unsupported extension.
The boundary is the C++ ABI tied to DuckDB's internal C++ objects. The demo
binaries used by these tests use that private ABI; Rust preserves their explicit
unsupported-load verdict. This does not exclude portable C ABI extensions,
signature/version validation, INSTALL behavior or C API callbacks from the goal.

These eleven original files exercise the private C++ demo entrypoints:

- `test/extension/concurrent_load_extension.test`
- `test/extension/consistent_semicolon_extension_parse.test`
- `test/extension/install_extension.test_slow`
- `test/extension/load_extension.test`
- `test/extension/load_test_alias.test`
- `test/extension/loadable_parser_override.test`
- `test/extension/reset_global_extension_option.test`
- `test/extension/test_alias_point.test`
- `test/extension/test_custom_type_modifier.test_slow`
- `test/extension/test_custom_type_modifier_cast.test`
- `test/extension/test_tags.test`

The current full Rust run reaches these explicit unsupported-load failures. Its
remaining results are still being collected; no new failure is accepted by name
similarity or by the extension suffix. Final review must inspect each raw failure.

## VSS persistence boundary

roastedDuck's compatible VSS implementation uses in-memory Rust HNSW indexes.
Cross-engine HNSW persistence is deferred under the current scope. Setting
`hnsw_enable_experimental_persistence=true` does not enable an unimplemented
persistent format. A database containing a C++ HNSW index can be opened and read
with that index left unbound; unsupported mutation/checkpoint binding is rejected.

Five unchanged pinned VSS files require persistent HNSW creation and retain raw
failures at that boundary:

- `test/sql/hnsw/hnsw_basic.test`
- `test/sql/hnsw/hnsw_crud.test`
- `test/sql/hnsw/hnsw_insert.test`
- `test/sql/hnsw/hnsw_insert_wal.test`
- `test/sql/slow/hnsw_reclaim_storage.test_slow`

The original `hnsw_experimental_persistence.test` only checks disabled-persistence
rejection and passes unchanged. The eleven other in-memory originals also pass.
Separately named memory supplements preserve selected SQL assertions from the
persistence files without converting their original raw failures into passes.
See [the VSS spec](../docs/vss-extension-spec.md) for source pins, all test verdicts,
recall results, predicate-expansion behavior, and measured shared SQL quirks.

## Uninitialized reference VARIANT padding

C++ 1.5.6 can include uninitialized bytes in a VARIANT BLOB when a NULL
primitive follows a non-NULL value during conversion. Its primitive converter
retains the previous payload size and advances the NULL row's offset without
writing those bytes. Public raw `hash(v)` consequently depends on allocator
contents for these inputs. Two identical native C++ processes differ in 423
of 5,000 hashes; the physical probe verifies that all differences from Rust
are confined to eight unused BLOB bytes after the final VARIANT_NULL entry.
Logical payloads/metadata match, and every normalized hash matches across both
C++ repeats and Rust. Rust initializes unused bytes and does not reproduce
process memory contents.

This exception is limited to raw hashes of those undefined padding bytes.
The 55 defined-value hash controls, a separate 5,000-row fully initialized
VARIANT workload's raw hashes, and logical grouping/join results match C++
literally. The original failed raw comparison remains preserved rather than
being normalized away. Evidence: external
`nested-hash-parity-20261027/{physical-2/padding-verification.json,
public-hashes-1/differences.json,public-hashes-2/verification.json}`.

## Reference constant state vector in ordered C API aggregates

DuckDB1.5.6's sorted aggregate fallback passes a CONSTANT_VECTOR to the underlying
update callback when no simple-update callback exists
(`src/function/aggregate/sorted_aggregate_function.cpp`). Its C API wrapper
(`src/main/capi/aggregate_function-c.cpp`, CAPIAggregateUpdate) reads that vector
with FlatVector::GetDataUnsafe without flattening the state pointers. The public
C callback receives a row count greater than one and an invalid state array.
In three20-row controls on the frozen build, state0 is valid and state1 is NULL.
A checked callback reports an error before dereferencing the invalid state.

The exception concerns the reference result oracle for multi-row argument-ordered
C API aggregates. Rust supplies valid repeated state pointers and verifies the
independent sums190/190,190/380 and190/190 for aliases, different updates and different
extra-data controls. Physical and serialized plans match C++ for all three.
Single-row ordered calls exercise the defined C++ path and match values/names/plans
literally. The50 paired native acceptance controls and three checked reference
fault controls are reported separately. Existing SQL tests/ratchets are unchanged.
Evidence: external `aggregate-equality-20261029/{before-6/,before-7/,
ordered-state-1/verification.json,native-matrix-1/verification.json}` and
`docs/aggregate-equality-parity.md`.

## Reference concurrent metadata failure

Native Mac ARM64 C++1.5.6 source-built reference934 also fails this original file
in current full1302 with `optional_ptr<CatalogEntry>::CheckValid()` unset. Current
audit1330 retains its raw verdict; native C++ unittest controls969/1257 reproduced
the mechanism independently. The current Rust full verdict is not yet available.

The earlier frozen Intel C++1.5.6 build fails
`test/sql/parallelism/interquery/concurrent_append_metadata_queries.test_slow`
with an unset optional catalog-entry pointer while concurrent threads create/drop
tables and query `duckdb_dependencies()`. The original canonical failure is retained.
The unchanged test also fails in DuckDB's own statically linked `unittest` runner:
one case,188 of189 assertions pass, followed by database invalidation from the same
optional-pointer error. A separate oracle-adapter execution reproduces the error.
This establishes the reference fault independently of the Rust adapter for this
Intel/Rosetta configuration; it does not generalize to every C++ platform/version.

In those earlier Intel controls, Rust passes the canonical test. This is a
reference-oracle limitation, not another accepted Rust failure. No assertion, SQL input or ratchet was changed, and no retry
was substituted for the original result. Inputs and signed artifacts verify after
the controls. Evidence: external
`platform-matrix-20261029/intel-metadata-reference-classification-1.json` and
`work/apple/intel-metadata-followup-1/` under that task.


## Recovered index keys deleted in the creating transaction

A separate native diagnostic writes a transaction that inserts local rows, creates
an index over a table with existing rows, and deletes existing key `v=42` before
commit. After WAL recovery, the full table multiset correctly excludes 42, but both
frozen C++ 1.5.6 and Rust reject reinserting that key with the identical unique
constraint error (type 18). This is a shared reference limitation, not a Rust-only
behavior difference and not a passing independent-correctness control. The original
failed diagnostics remain unchanged; no SQL expectation or ratchet was weakened.
Evidence: external `art-persistence-parity-20261029/diagnostics-1/` and
`docs/art-persistence-parity.md`.

A different reference fault occurs when an unbound expression index receives later
WAL mutations and shutdown checkpoints it before binding. C++ can persist a stale
tree while discarding its WAL. Rust now retains the recoverable WAL until a binding
connection can apply the mutations. Explicit CHECKPOINT binds first. The separate
28 defined cross-engine recovery journeys pass, including repeated Rust shutdowns,
receiver writes/checkpoint and original-engine readback. The shared premature-close
failure is preserved in `expression-wal-before-4/`, rather than counted as a pass.


### Current C++ reference concurrency observations (2026-11-03)

The source1959/core1961 paired macOS corpus retains two raw C++ failures:

- `concurrent_append_metadata_queries.test_slow`: scanning dependencies dereferences
  an unset optional catalog pointer and invalidates the C++ database. This is the
  previously documented reference failure; its current worker log preserves the
  original error and native stack.
- `test_concurrent_drop.test`: one of the original 100 concurrent iterations gets a
  catalog write-write conflict while dropping `seq`, whereas the test accepts only
  the dependency error. The fresh Rust full run passes. Eight focused replays of
  the unchanged case pass on each engine. These later passes do not replace the
  original C++ full-run failure.

Both failures remain visible in the C++ totals (4,838 pass, 30 skip, two fail).
Rust's totals are 4,826 pass, 30 skip and the same 14 previously documented raw
failures. Review2022 classifies all 76 nonpasses, with no unexplained result or skip
differences. Its derived comparison maps only one exact canonical JSON input URI
between fresh fixture roots, after checking identical file hashes and all other
failure fields. No SQL, expected result or raw verdict is changed. These are scoped
corpus observations, not whole-release or performance acceptance.

Evidence is on the external SSD under
`.context/artifacts/platform-matrix-20261029/`: `macos-full-corpus-1967/`,
`cpp-concurrent-drop-diagnosis-2007/`, and `current-macos-full-corpus-review-2022/`.
