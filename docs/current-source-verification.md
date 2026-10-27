# Current-source verification

Updated 2026-10-27. **The port goal remains active.** The current source-5 overlay has
909 entries and 908 verified build inputs. The fresh full 4,870-file SQL run passes its documented-exception gate;
source-5 aggregate performance measurements fit the approved tradeoff, with outlier
repeats and final-artifact acceptance still pending. Earlier historical
checkpoints do not substitute for these gates.

Required clients are Python, Go and Swift. C API/ADBC are preserved. Other language
clients are outside the owner's goal. The target is frozen DuckDB 1.5.6. Preserve the
accepted uncached lower-memory tradeoff; see [performance policy](performance-acceptance.md).

| Current checkpoint | Verified result |
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
The completion audit nevertheless identifies required functional work: the Parquet writer
still rejects supported footer-key encryption. The `column_keys` rejection matches frozen
C++ and is not a request to exceed its functionality. This gap and other open contract
items prevent declaring the overall port complete despite the green scoped gates.
