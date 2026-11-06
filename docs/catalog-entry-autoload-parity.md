# Catalog-entry autoload and active extension settings

The bare539-file COPY comparison1001 matched21 skips, including19 requiring
HTTPFS. Those matching skips did not verify the encrypted database/WAL cases.
A private authenticated extension repository exposed13 real Rust autoload failures
in34 original files: C++ passed all34 while Rust passed21. No original test or
ratchet was edited to remove those failures.

## Behavior and correction

DuckDB1.5.6 `Catalog::AutoLoadExtensionByCatalogEntry` finds the known provider of a
missing function, COPY format, type or collation, checks autoload policy, loads it,
and retries catalog lookup. The corresponding GetEntry paths retry even lookups
that otherwise return NULL. The local reference is the frozen1.5.6 source used by
native ARM64 release comparator934 (`src/catalog/catalog.cpp`, lines622 and1080).

The planner now delegates provider registration through DatabaseState and retries
its original search path once. Registration uses the Rust database's compiled
providers; it does not execute a C++ engine or parse an error string to infer a
provider. Disabled autoload and mismatched entry types retain their typed lookup
behavior. Table/scalar/aggregate/pragma grouping follows C++ catalog matching.

PRAGMA expansion binds before ordinary statement startup, so that path now
publishes the current settings snapshot before lookup. A second original-file
failure exposed options added during binding: loading Parquet registered
binary_as_string after the statement had captured its settings. Snapshots now use
a weak database reference to include newly registered option defaults. Captured
existing values and session overrides are preserved. The weak reference avoids
retaining the database through statement settings. No query/table/object cache or
COPY memory-policy change was introduced.

C++1017 independently confirms a diagnostic side effect: SELECT dbgen(0) loads
TPC-H while resolving the table-function-used-as-scalar error. The new test checks
that behavior explicitly. CAPI1016 began with TPC-H already loaded and is retained
as an unsuitable control for the before/after transition, not counted as proof.

## Same-source signed development evidence

| Evidence | Result |
| --- | --- |
| Strict workspace/all-target Clippy1026 | Pass |
| Fresh test build1027, signed bundle1028/runtime1029 |9 new autoload/settings +3 existing overload cases pass |
| Fresh native build1030 | Actual compiler success before signing/execution |
| Shared1031 and SQL1032 | Strict native signatures and pinned whole-file CMS pass |
| All34 original encrypted/HTTPFS files1033 | C++34pass, Rust34pass, zero skips/failures/crashes/timeouts/resource kills |
| Four original encrypted WAL files | All pass on both engines |
| Full public C API1034 |30 files/155 cases/814832 assertions pass; selected native image and strict signatures verified |

Signed development shared SHA256:
`a01119542ce034f09e76d532f59d271d305b89b766101e4654eee2960965bbea`.
Source1036 archives2372 workspace inputs. Audit1035 verifies1468 compiled project
inputs, authenticated artifacts, exact original-file results and completed owned
jobs. There are547 duckdb_ exports and no private ADBC test hooks. This is a Mac
ARM64 development component, not the final signed release.

The nine new cases cover table-function and pragma autoloading, COPY format lookup,
disabled autoload and later enablement, wrong-scalar diagnostic behavior, disabled
pragma autoload, registration surviving user rollback, visibility in an already-
open transaction, and local Parquet replacement scanning with new defaults and
preserved session overrides. Existing typed-overload/ICU and identity cases also
pass. Original inputs and expected results remain unchanged.

Repository1033 contains nine symlinks to the authenticated frozen934 built-in
modules and590 official Mac HTTPFS package. The SQL runtime uses per-test private
extension and secret directories; it does not install into the user's extension
directory. Same5.6GiB engine budget,7GiB sampled RSS guard,300seconds/file and serial
jobs apply to both engines. Source, all canonical fixture bytes/symlink targets,
and signed reference/tools/HTTPFS package verify before and after each collection.

## Preserved failures and individual timing outliers

- Incomplete repository1008: both engines21pass/13fail due to missing modules.
  It is preserved separately from the corrected repository's results.
- Correct repository1009: C++34pass; Rust21pass/13 real missing-provider failures.
- Initial signed tests1014:5/7 new +3 existing pass. One implementation issue was
  PRAGMA settings publication; the wrong-type expectation was corrected against
  independent C++1017. The initial failures remain preserved.
- Partial correction1024: C++34pass; Rust33pass/1 failure for the newly registered
  binary_as_string default. Final1033 passes that exact unchanged file.

These are single whole-file development-Rust versus release-C++ observations,
including setup and network work, not optimized query-only performance acceptance:

| Original file | C++ ms | Rust ms |
| --- | ---: | ---: |
| copy/encryption/tpch_sf1_encrypted.test_slow |5650|9161|
| copy/parquet/delta_byte_array_fixed_variable.test_slow |2390|4924|
| storage/encryption/temp_files/encrypt_asof_join_merge.test_slow |380|1285|
| storage/encryption/temp_files/encrypted_offloading_block_files.test_slow |42136|86622|
| storage/encryption/temp_files/encrypted_tpch_join.test_slow |18559|29161|

All15 file ratios above1.25, with available sampled RSS and explicit missing samples,
are retained in `catalog-autoload-settings-audit-1035/all-file-time-outliers.json`.
RSS samples are0.5-second observations, not exact peaks. Historical1009 offloading
was87595ms;1024 was85975ms. Those retained measurements do not establish a
controlled optimization delta or waive the individual final-release outlier gate.

## Remaining goal acceptance

The original bare corpus968 and prior CSV539-file component remain historical
receipts on their own immutable source/images. Do not combine them with this
component into a final same-source release count. Current CSV implementation code
is unchanged from source1003; its memory duplication fix remains preserved.

This cohort verifies original encrypted database operations and same-engine WAL
cases. Subsequent encrypted matrix1041 verifies16 cross-engine checkpoint/nonempty-
WAL mutation/readback journeys and32 incomplete-tail recoveries across GCM/CTR x
v0/v1 on these same development images; see
`docs/encrypted-storage-cross-engine-parity.md`. Final release artifacts still need
their own bidirectional encrypted persistence refresh. Broader core/HTTPFS lifecycle, redirect/version/proxy and credential
behavior, final whole SQL/shell/C API/ADBC, Python/Go/Swift/platform matrices, and
coupled native TPC-H/ClickBench time/RSS/outlier acceptance remain open.

The owner explicitly permits third-party native crypto while retaining a Rust
DuckDB engine. Preserve uncached release_on_unpin, the accepted lower-memory
performance policy and the conditional memory-bound COPY exception. Mac Intel and
additional language clients remain outside scope. The full goal is active/incomplete.

Evidence is on the external SSD under
`.context/artifacts/platform-matrix-20261029/`, principally1007–1036.
