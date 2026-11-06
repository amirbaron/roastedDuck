# DuckDB Rust source2551 software candidate

The goal is **complete under the owner-approved scope**. All 22 scoped software collection/review requirements are evidenced. The owner explicitly instructed “Record a physical-device test exception”; actual iPhone/iPad execution and provisioning remain **unverified**. The [new completion audit](../.context/artifacts/platform-matrix-20261029/owner-approved-delivery-2934/requirement-completion-audit.json) records that exception separately from software evidence. All raw SQL, shell and performance exceptions remain explicit.

## Delivery

- [Candidate archive](../.context/artifacts/platform-matrix-20261029/owner-approved-delivery-2934/duckdb-rust-source2551-candidate-20261106.tar.gz): 1,944,773,808 bytes, about 1.81GiB.
- [Signed manifest](../.context/artifacts/platform-matrix-20261029/owner-approved-delivery-2934/SHA256SUMS), [signature](../.context/artifacts/platform-matrix-20261029/owner-approved-delivery-2934/SHA256SUMS.csig), [verification script](../.context/artifacts/platform-matrix-20261029/owner-approved-delivery-2934/verify-detached-manifest.py).
- [Archive/extraction proof](../.context/artifacts/platform-matrix-20261029/release-distribution-2924/verification.json): all 300 original payload files and symlink targets match after extraction; engine and original manifest bytes are unchanged.
- [Final runtime audit](../.context/artifacts/platform-matrix-20261029/final-signed-payload-audit-2921/verification.json): 24 roles, 15 normal wheels, three ARM-only Apple slices, actual relocated library paths and portable CLI execution.

Archive SHA256: `42367d8063aab3e7a5729a940bd21ecc06bc66b62a9cb55533a2f25fbd2143c5`.

Inner manifest SHA256: `309821a4b830e46a8f1cb628dd43e83e9e93bf0d041dd716c34db49cec081bb2`.

Public certificate SHA256: `a1354a5ea7a95b11430e91e216689c84e8c1a6392b4c06466199b87217cb709d`.

The candidate has not been publicly published. Keep all included project, upstream and third-party notices, including TPC, ICU, ZSTD and native crypto notices. The DuckDB engine is Rust; third-party native crypto is explicitly allowed.

Required clients are Python, Go and Swift; public C API/ADBC are preserved. Targets are macOS arm64, iOS arm64 device/simulator and Linux arm64/x86_64. Intel Mac, other Apple platforms, Windows and additional clients are outside this goal. Go builds require `duckdb_use_lib,duckdb_arrow` tags and linkage to the selected Rust native package; the upstream default prebuilt C++ path is outside the verified contract.

## Verification

| Scope | Current recorded evidence |
|---|---|
| macOS SQL | All 4,870 files: 4,826 PASS, 30 SKIP, 14 retained raw FAIL. C++ has 4,839 PASS, 30 SKIP and one retained reference failure. No unclassified nonpasses or skip differences. |
| Linux SQL | All 4,870 files on each architecture: 4,828 PASS, 28 SKIP, 14 retained raw FAIL. The x86_64 reference worker crash remains explicit. |
| Shell | Full 395 upstream cases, 768 differential cases, 46 PTY sessions, 21,460 original byte cases and 399 native Linux filename obligations per target reviewed. Four supplemental original HTTP cases actually pass. Universal byte parity is false; metadata, native frames and reference incidents remain. |
| Workspace | 149 fresh signed harnesses: 1,089 PASS, no failures and seven retained ignores. Two required ignored CAST/RE2 controls ran separately; five diagnostics remain unpassed. |
| C API/ADBC | 30 harnesses/155 public cases and 64 ADBC cases/3,040 assertions per engine/target, plus feature-free public consumers. Shipping libraries retain 547 public exports and no private test hooks. |
| Python | Five Mac and ten Linux normal CPython 3.10–3.14 cells independently qualified. Linux has 62,270 XML outcomes per engine: 61,486 PASS/784 matching nonpasses, plus 72 WAL journeys, 432 encryption cases, 36 installed ADBC environments and 80 typing runs. All 36 Linux workloads/216 samples remain individual. |
| Go/Swift | Full Go 652-case and Swift 133-case suites per engine/platform, plus independent typed persistence journeys. Final relocation checks cover four Mac Go journeys/12 processes, two Swift consumers, five Mac wheel installations and both Linux C API/public ADBC targets. |
| Apple SDK | Three ARM-only signed slices; 133 actual simulator Swift cases, typed consumer and four cross-engine WAL journeys on unchanged SDK bytes. Physical execution/provisioning remains unverified. |
| Storage/codecs | 60 typed journeys, 120 tail recoveries, 330 native processes and 210 headers across Mac and both Linux targets. Codecs add 36 FSST/DICT_FSST journeys, 60 immutable inputs, 60 ZSTD Parquet files, 120 reads and 108 processes, including 70MiB level-22 integration. |
| Native performance | All 130 TPC-H SF10/ClickBench query/thread configurations at 1/14 threads, one warmup and three timed repetitions. Independent numeric review and fresh full-result controls for all 13 tied configurations pass. COPY's 24 samples and 30 original whole-file samples remain separate. |

The [live ledger](../.context/artifacts/platform-matrix-20261029/completion-evidence-map-2551.json) pins current receipts. The [Linux Python index](../.context/artifacts/platform-matrix-20261029/current-Linux-Python-all-ten-index-2892/verification.json), [storage index](../.context/artifacts/platform-matrix-20261029/current-storage-codecs-capacity-index-2850/verification.json) and [performance report](performance-final-native-20261106.md) retain details. Linux x86_64 functional execution uses translation; native x86_64 performance is not claimed.

## Performance boundaries

The historical geometric-mean policy is unchanged. Best-time geometric means are 7.2%/14.7% longer for TPC-H and 11.4%/15.6% longer for ClickBench. Adjusted geometric-mean RSS reductions are 78.1%/70.6% and 69.1%/62.2%; raw whole-process reductions are 75.5%/68.7% and 66.5%/60.1%. No cache or memory budget was increased.

ClickBench's summed best times remain 22.9%/25.4% longer and summed medians 21.6%/22.7% longer. Those totals do not meet a strict 20% cap. Its one-thread maximum RSS is 13.270GB C++ versus 13.116GB Rust, almost equal. All 63 flagged configurations and all 130 rows remain visible.

Whole-file VARIANT is 5.391x time/0.376x RSS; batch reclaim 3.115x/0.774x; CSV 1.494x/1.102x; checkpoint/index 3.042x/0.593x; primary-key reclaim 2.509x/0.977x. Single-thread COPY is 37.5% slower with 46.0% more median RSS. The approved 14-thread/256MB COPY condition is met; failed C++ skew attempts are not credited as successful timing comparisons.

These limitations, all raw SQL failures, matching skips/xfails, reference crashes, shell differences, upstream Swift language-mode warnings and ignored diagnostics remain in [known exceptions](release-known-exceptions.md). No raw failure was rewritten as a pass and no uncertain reference-crash cause was inferred.

## Owner-approved physical-device test exception

The owner explicitly accepted this exception on November 6, 2026. `ios_physical` remains unverified: no actual iPhone/iPad execution, provisioning or physical-runtime identity/typed-runtime evidence is claimed. Signed device builds and actual simulator results are separate verified evidence. [The signed disposition](../.context/artifacts/platform-matrix-20261029/owner-approved-delivery-2934/STATUS.md) and [exception record](../.context/artifacts/platform-matrix-20261029/owner-approved-delivery-2934/physical-device-test-exception.json) preserve the exact approval.

The archive, original inner signatures and historical creation status are unchanged. The new signed delivery addendum supersedes the earlier unwaived hardware disposition; it does not convert hardware testing into a pass or waive any other requirement.
