# Current-source verification

Updated2026-10-29. **The full port goal remains active.** The current signed native
candidate is external `aggregate-equality-20261029/source-5/manifest.json`, with961
overlay entries/960 verified build inputs. It includes the complete current overlay,
EXPORT fixes, all ZSTD strategies/splitting/automatic LDM, FSST candidate/heap fixes,
and aggregate callback/bind-data/logical-type equality corrections. The engine
contract remains DuckDB1.5.6. Required clients remain Python, Go and Swift only;
preserve C API/ADBC and the accepted uncached performance policy.

## Current aggregate/native integration checkpoint

| Gate | Actual current-source evidence |
|---|---|
| Workspace/all-target Clippy | Pass, warnings denied (`clippy-5-terminal.json`) |
| Signed targeted SDK debug/release |36 complete frozen C++ physical plans/values/raw names plus eight return-type controls each |
| Signed broader affected crate tests |142pass across22 executables; one existing optional full cast-oracle CLI tool ignored |
| Signed native images | Actual build exited0; frozen CLI/library/SQL runner strictly signed;547 C API exports/zero ADBC test hooks |
| Public C ABI native matrix |50 complete physical/serialized plans, values, metadata, empty/frame/ordered/callback/extra-data controls match C++ |
| Checked reference defect | Three multi-row ordered C API calls receive invalid C++ state arrays; Rust correct sums and literal physical/serialized plans verified separately |
| Affected SQL |324 files:323pass/one exact C++ longdouble skip; zero failures/crashes/timeouts/verdict regressions |
| Full C API suite |30files/155cases/747436assertions pass; each loader selects the immutable current library |
| Full workspace unit/SQL/shell/clients/native ADBC/persistence/WAL/performance | Final refresh required; prior results are controls |

Current immutable library SHA256:
`e9b447335ee2c5fba4bcb399092d74676e85284a1acdfa769ecd0f2d62c2372d`.
CLI: `6798e2fec6736382a74900dfb43663a92e83b8be145e8cb82d1795cf1e5409e8`.
SQL runner: `846188c762c3801ae79a1f819bbff6ebe872f840473c1d524d07201e3e3dd301`.
Evidence: external `aggregate-equality-20261029/` and
[aggregate report](aggregate-equality-parity.md). The narrow C++ ordered-state
reference defect is explicit in `conformance/divergences.md`; the50 paired controls
remain literal comparisons, separate from reference-fault diagnostics.

Retained FSST component evidence:73 exact corpora/146 cross-decodes,124 signed
common/storage tests per profile,190 affected SQL files,155 C API cases and12
bidirectional FSST/DICT_FSST checkpoint/nonempty/torn-WAL mutation/readback journeys,
with20 immutable raw inputs. See [FSST report](fsst-parity.md). Earlier codecnative
controls remain external `zstd-strategy-parity-20261027/`:25/25 signed codec tests per
profile,2072 explicit frames and20 cross-engine Parquet files including70MiB LDM
values. See [codec report](zstd-strategy-parity.md). Actual SSD reads/writes succeed.

The confirmed EXPORT gaps were closed at the retained before-codec checkpoint:
160/160 complete public plans match C++; all14 signed SDK cases, eight cross-engine
exports/imports and installed Arrow19/21 registered filesystem checks pass. See
[EXPORT evidence](export-plan-parity.md). That component evidence does not
substitute for final current-source release verification.

Remaining contract audits in `work-queue.md` still need authoritative disposition:
native Arrow producer
projection/filtering, ART legacy/expression/WAL paths, nested VARIANT statistics,
Parquet schema evolution, vector-heap accounting, configured extensions/platforms.
Verify each against actual current code/runtime; stale backlog prose alone neither
proves a gap nor closes it. Full final SQL/shell/selected-client/ADBC/persistence/WAL
and coupled TPC-H/ClickBench/outlier gates remain required. No goal completion is
claimed by this aggregate/native checkpoint.

## Retained before-codec integrated control

The following batch-copy source8 checkpoint predates the current native codec image.
It remains historical integrated evidence, not current-image release acceptance.

### Batch-copy source8 gates (before-codec)

| Source-8 checkpoint | Verified state |
|---|---|
| Workspace/all-target Clippy | Pass, warnings denied (`clippy-build-6`) |
| Signed ordered-COPY SDK | Four passes (`sdk-verification-4`), including pressure, ordering and failure recovery |
| Signed shipping artifacts | Strict signatures; 547 C API exports; zero ADBC test hooks (`native-images-2`) |
| C API | 30 files/155 cases/725,896 assertions; actual selected-library loader paths verified |
| Supplemental encrypted-Parquet SQL | Both tests pass on Rust and C++, including encrypted SF1 export/import and 22 TPC-H answers through tables/views |
| Coupled COPY diagnostics | 36 samples completed against C++ and preserved old Rust; all 33 successful files independently cross-read for every payload and order |
| Expanded affected SQL | 1,059 files: 1,047 pass, 11 exact C++ skips, one documented avoided-OOM assertion; all verdicts and failure payload unchanged |
| Full 4,870-file SQL / full unit suite | Fresh final-candidate verification still required |
| Shell upstream | Fresh Rust/C++ 392 passes/three exact shared skips using existing SSD lexical-alias fixture; initial shared unquoted-space-path failures retained |
| Shell differential/PTY | Full source-8 768 cases/46 PTY pass: zero unexpected differences, one known native-frame-body exception |
| Python Arrow 19/21 fast | 3,241/3,492 records, exact C++ IDs/verdicts/skip reasons; signed source-8 wheel installed in both environments |
| Python non-fast / installed ADBC | Fresh non-fast 71 pass/eight shared skips; both installed ADBC/file/UDF handoffs pass; 547 API exports/zero test hooks |
| Python persistence/WAL | Eight 15-column append/checkpoint/nonempty-WAL/mutation/readback journeys pass across both Arrow environments |
| Go | Complete pinned root suite with Arrow: 652 passes, exact C++ IDs/verdicts; 79 source files and actual selected DLL verified |
| Swift | Fresh Rust/C++ 132 upstream plus identity pass with exact IDs; relative framework, relocated/standalone consumers and four checkpoint/WAL journeys pass |
| Native ADBC | Fresh Rust/C++ full 64 cases/3,040 assertions each pass with exact ordered IDs; feature-free shipping restored/verified before harness; isolated bridges and feature Clippy verified |
| Native/client persistence/WAL | Refresh required on final images; prior scoped journeys remain controls |
| Full TPC-H/ClickBench timing/RSS/outliers | Final-candidate measurement and documented acceptance still required |

The immutable signed shipping library SHA-256 is
`2c96baa818920f6be19daaebea5ed3e3cbcf833a0a308139329ff0184bebca0c`.
The CLI is `6527e7f76b907296ff05c5f2a019407480065e04fb49c5561b554850a82a88ec`;
the SQL runner is `02e16760f696206437040c7104c239a67539150879417b0070f21f4f801e05a7`.
Evidence is under external `batch-copy-memory-20261027/`, including source, signatures,
SDK/C API, `encryption-sql-2`, `after-probe-2` and accepted `affected-sql-2` receipts. The source-8 Python native image is
`a6d88586d08536c79addc1d9545f891145165d876a8c7ce83df975b6293f4d14`.
Its fast/non-fast/installed-ADBC and persistence receipts are before-codec scoped evidence;
applicable broader contract/platform/extension audits remain.

COPY is an explicit performance outlier. At 14 threads/256 MB the corrected path is
303.0 ms/459.5 MB versus C++ 246.6 ms/463.8 MB, and the old unaccounted Rust path
87.4 ms/665.0 MB. At 512 MB it is 222.5 ms/565.0 MB versus C++ 152.9 ms/683.8 MB.
CPU concurrency follows C++ budgeted throttling. On2026-10-29 the owner explicitly
accepted this documented tradeoff, conditional on final TPC-H/ClickBench results
remaining within the approved speed–memory range. Individual outliers remain
visible, and final artifact measurements are still required. The first shipping
candidate's larger runtime regression remains rejected and preserved.
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

## Upstream shell path calibration and current pass

The first source-8 upstream run produced nine failures because the upstream tests
interpolate temporary filenames into unquoted dot commands, while the physical SSD
path contains spaces. C++ reproduces the identical 395-case verdict set and the
same nine failures. That counterexample is retained separately.

Using the repository's existing `shell_ssd_tmp_path` fixture, both engines now
pass all 392 upstream cases with the same three skips and literal skip reasons.
The fixture delegates pytest's original lifecycle and returns only a whitespace-free
workspace symlink path to the same verified external storage. No test body,
assertion or shell tokenization changed. JUnit collection-path prefixes are mapped
only before the fixed `tools.shell.tests` module names; case names/verdicts remain
unchanged. All 909 source inputs, native CLI hashes and signatures verify stable.

Evidence is external `batch-copy-memory-20261027/{shell-upstream-1/path-control-comparison.json,
shell-upstream-alias-verification-2.json,shell-upstream-rust-alias-2/,
shell-upstream-cpp-alias-2/}`. Full 768-case/46-PTY differential verification now passes on the source-8 CLI
with zero unexpected differences and one native-frame-body exception. Case/driver
hashes, all 909 inputs and both CLI signatures verify unchanged.

## Next confirmed functionality gap

A fresh public C++/Rust probe confirms that CSV/Parquet EXPORT logical plans
serialize successfully on C++ while Rust returns NotImplemented. See
[EXPORT plan contract](export-plan-parity.md). The green source-8 component/client/
shell gates do not establish whole-port completion while that supported gap and
other contract/final-performance gates remain open.
