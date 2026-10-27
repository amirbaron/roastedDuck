# Current-source verification

Updated 2026-10-27. **The port goal remains active.** The live candidate is
`batch-copy-memory-20261027/source-8/manifest.json`: 910 overlay entries and 909
verified build inputs. It includes the import/native-byte work, footer-key Parquet
encryption, nested/VARIANT hash fixes and the complete ordered-COPY helper lifecycle.
Required clients are Python, Go and Swift; preserve C API/ADBC. Other language clients
remain outside the owner's scope. The frozen engine contract is DuckDB 1.5.6.

## Live candidate gates

| Source-8 checkpoint | Verified state |
|---|---|
| Workspace/all-target Clippy | Pass, warnings denied (`clippy-build-6`) |
| Signed ordered-COPY SDK | Four passes (`sdk-verification-4`), including pressure, ordering and failure recovery |
| Signed shipping artifacts | Strict signatures; 547 C API exports; zero ADBC test hooks (`native-images-2`) |
| C API | 30 files/155 cases/725,896 assertions; actual selected-library loader paths verified |
| Supplemental encrypted-Parquet SQL | Both tests pass on Rust and C++, including encrypted SF1 export/import and 22 TPC-H answers through tables/views |
| Coupled COPY diagnostics | 36 samples completed against C++ and preserved old Rust; all 33 successful files independently cross-read for every payload and order |
| Expanded affected SQL | 1,059 frozen files currently running; final verdict pending |
| Full 4,870-file SQL / full unit suite | Fresh final-candidate verification still required |
| Shell | Fresh final-candidate upstream/differential/PTY verification still required |
| Native ADBC / Python / Go / Swift | Final-candidate rebuilt packages and complete selected-client gates still required |
| Native/client persistence/WAL | Refresh required on final images; prior scoped journeys remain controls |
| Full TPC-H/ClickBench timing/RSS/outliers | Final-candidate measurement and documented acceptance still required |

The immutable signed shipping library SHA-256 is
`2c96baa818920f6be19daaebea5ed3e3cbcf833a0a308139329ff0184bebca0c`.
The CLI is `6527e7f76b907296ff05c5f2a019407480065e04fb49c5561b554850a82a88ec`;
the SQL runner is `02e16760f696206437040c7104c239a67539150879417b0070f21f4f801e05a7`.
Evidence is under external `batch-copy-memory-20261027/`, including source, signatures,
SDK/C API, `encryption-sql-2`, `after-probe-2` and active `affected-sql-2` receipts.

COPY is an explicit performance outlier. At 14 threads/256 MB the corrected path is
303.0 ms/459.5 MB versus C++ 246.6 ms/463.8 MB, and the old unaccounted Rust path
87.4 ms/665.0 MB. At 512 MB it is 222.5 ms/565.0 MB versus C++ 152.9 ms/683.8 MB.
CPU concurrency follows C++ budgeted throttling. The user preference question about
this documented tradeoff is pending; no answer or approval is inferred. The first
shipping candidate's larger runtime regression remains rejected and preserved.
See [COPY measurements and lifecycle](batch-copy-memory-parity.md).

The accepted lower-memory TPC-H/ClickBench policy remains unchanged; no table cache
was introduced to close small speed gaps. Preserve individual query outliers and
whole-process plus interpreter-adjusted RSS. See [performance policy](performance-acceptance.md).
A narrow C++ uninitialized-VARIANT-padding raw-hash exception is documented in
`conformance/divergences.md`; defined payloads, normalized hashes and independent
file journeys have separate evidence in [nested hashing](nested-hash-parity.md).

## Before-stage complete integrated control

The following import-source-5 checkpoint predates encrypted-Parquet/nested-hash/COPY
changes. Its 909-entry/908-input overlay, full SQL/shell/client/runtime/packaging/
persistence and performance evidence remain preserved, and do not substitute for the
new candidate's final gates. Its Python image and all other identities below belong
to that before-stage checkpoint.

| Prior import-source-5 checkpoint | Verified result |
|---|---|
| Workspace/all-target Clippy | Pass, warnings denied |
| Signed import/registered-storage SDK | 15 pass |
| Targeted SQL | 785 files: 767 pass, two existing failures, 16 exact C++ skips; all prior verdicts unchanged |
| Full SQL corpus | 4,781 pass / 15 existing documented failures / 74 exact C++ skips; zero crashes/timeouts, zero ratchet regressions |
| Upstream shell | 392 pass, three unchanged skips |
| Shell differential | 768 cases / 46 PTY, zero unexpected differences; one native-frame-body exception |
| C API | 30 files / 155 cases / 744,074 assertions; selected signed library verified |
| Native ADBC | 64 cases / 3,040 assertions on both engines, same case order/adaptation hashes |
| Python Arrow 19 fast suite | 3,241 records: 3,205 pass, 27 skip, eight xfail, one existing XPASS; exact C++ IDs/verdicts/skip reasons |
| Python Arrow 21 fast suite | 3,492 records: 3,455 pass, 28 skip, eight xfail, one existing XPASS; exact C++ IDs/verdicts/skip reasons |
| Python installed import journeys | 14 literal C++ matches per Arrow environment, including registered-memory imports and failure/rollback behavior |
| Installed Python/ADBC | File/UDF/parameter/rollback/commit/reopen journeys pass; two additional import controls exact C++ per environment |
| Python runtime/stubs | 385 runtime exports / 29 typing-only declarations, presence exact C++; seven stubs and all 300 frozen test/data files verified |
| Go | 652 pass, exact frozen C++ case IDs/verdicts |
| Swift | 132 upstream plus one identity case pass on both engines; case IDs match |
| Swift distribution | Current relative framework, relocated package and standalone consumer pass with only the selected Rust image |
| Cross-engine persistence/WAL | Four scalar-expression, four Swift consumer and four 15-column Python append journeys pass; separate scoped cohorts |

The native shipping library is strictly signed and exports 547 C API functions with
zero ADBC test hooks. The ADBC test feature was copied to a separate library; the
feature-free shipping build was restored and checked **before** its harness ran.
The immutable shipping library SHA-256 is
`38603874f849b13c24ee23ec8cd6107294860c602bda19d63d3ad87bf8df622a`.
The signed Python image SHA-256 is
`2d7481420e5015748dea8ef27bca89fc3972c721103b9b4253cef60868bfa05b`.

The integrated evidence index is external
`import-preprocessing-20261027/verification-checkpoint-2.json`, including the accepted full-corpus receipt.
The full corpus has its own immutable runner/source/reference/fixture manifest and logs
under `full-corpus-1/`. The 15-column journeys preserve database and nonempty-WAL snapshots
before replay and verify the actual native image before/after every child process.

This is current-host macOS arm64 evidence. It does not silently close broader API
signature/overload/lifetime/concurrency/backend/platform audits, configured-extension
coverage, or earlier explicitly documented reference defects. Shared skips do not prove
unavailable extension functionality. Later catalog numeric dependency/registration-order
differences remain documented separately; startup main/schema identity and the literal
shell database-list difference are closed. The obsolete OID waiver is removed.

The full corpus has exact file coverage and every prior verdict is unchanged. All failure
payloads match except two occurrences of the generated worker-directory path in the
documented avoided-OOM caching-operators case; the exact mapping and original rejected
strict receipt are preserved. All 908 source/runner/reference/ratchet hashes, signatures
and 4,870 SQL/1,522 data fixture identities verify unchanged.

Reconcile remaining contract evidence and exceptions, and record coupled
TPC-H/ClickBench timing and memory against the preserved
uncached baseline and C++, with individual query outliers. Exact performance parity or
outperformance is not required. No table cache was introduced to erase small speed gaps.


The source-5 performance checkpoint now checks all 130 TPC-H/ClickBench query/thread
configurations and fits the approved aggregate policy; see [measured performance](performance-acceptance.md).
That before-stage checkpoint predates the encrypted-Parquet, nested-hash and ordered-COPY
changes now under verification. Supported footer-key encryption is implemented and has
independent cross-read/corruption/SQL evidence; column_keys retains the shared C++ limit.
The newer core/client/final-performance gates below must be completed on their own images.
