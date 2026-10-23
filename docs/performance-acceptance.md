# Performance acceptance

Policy approved by the owner on 2026-10-23. Final-binary verification is pending.
This policy supersedes both the original outperformance targets and the later
independent speed/memory parity gates.

Preserve the measured lower-memory baseline. Roughly 15–20% longer execution time
with 60–70% less memory on TPC-H and ClickBench is acceptable. Do not increase memory
merely to match C++ caching or close a small speed gap. Evaluate both metrics and
report individual query outliers separately. Correctness, functional parity and
storage/WAL compatibility remain required.

## Preserved comparison baseline

Use the checked round-6 integrated **uncached** results in
`.context/artifacts/parity-20261022/bench/`, as summarized in `docs/perf-backlog.md`.
The earlier `r6base-hits-t1` aggregate is incomplete and must not substitute for this
baseline. The checked run contains 160 passing result comparisons across eight suite /
thread configurations. These are historical baseline measurements, not measurements
of the current unfinished identifier migration.

Speed is C++ wall time divided by Rust wall time. Memory is Rust/C++ peak RSS above
the Python interpreter; retain whole-process RSS as well when making new comparisons.
The metrics below are suite geometric means, not ratios of summed wall time.

| Suite | Threads | Speed | Memory |
|---|---:|---:|---:|
| TPC-H SF10 | 14 | 0.88× | 0.29× |
| TPC-H SF10 | 1 | 0.94× | 0.21× |
| ClickBench | 14 | 0.87× | 0.36× |
| ClickBench | 1 | 0.91× | 0.29× |
| h2o group-by | 14 | 1.03× | 0.72× |
| h2o group-by | 1 | 0.99× | 0.40× |
| h2o joins | 14 | 1.16× | 0.79× |
| h2o joins | 1 | 1.26× | 0.50× |

The preserved uncached `rustdb`, `slt` and `libduckdb.dylib` binaries remain in
`.context/artifacts/parity-20261022/uncached-merged/`. Their freshly recorded SHA-256
identities, the rejected cache patch and its source are in
`.context/artifacts/performance-policy-20261023/policy-manifest.json`. Do not overwrite
these controls or reuse their identity for a subsequently rebuilt binary.

## Individual baseline outliers

These existing outliers remain visible even though the owner accepts the overall
lower-memory tradeoff. They are not automatically evidence of a new regression.

| Query | Existing baseline observation | Required follow-up |
|---|---|---|
| TPC-H q05, 14 threads | 0.73× speed | Report repeat measurements and final-vs-baseline change |
| TPC-H q21, 1 thread | 0.79× speed | Report repeat measurements and final-vs-baseline change |
| ClickBench q19, 14 / 1 threads | 0.42× / 0.39× speed | Keep this substantial slowdown separate from suite means |
| ClickBench q28, 1 thread | 44.8 s versus C++ 30.7 s, 0.69× speed | Retain timings, result checks and memory together |
| ClickBench q00 | About 3.2 MB extra whole-process peak RSS; 1.11× after interpreter subtraction | Report absolute bytes and the corrected/interpreter-adjusted ratios |

The old full table-cache candidate increased residency markedly and had additional
memory outliers (TPC-H q21 and ClickBench q22/q28). It is rejected under the owner's
policy. Its uncommitted `keep_cached()` switch is removed; persistent table segments
again use `release_on_unpin()`. Historical cache benchmarks and diagnostic artifacts
are retained for comparison, not as a performance target.

## Final verification and sign-off

Completion requires a report on the actual final signed binaries, with:

- Source, compiler/build options, signatures and binary hashes; data/query identities,
  thread/memory settings, repetitions and measurement conditions.
- Correct results before a query's timing counts; failed/error/mismatched queries must
  fail the run rather than disappear from an average.
- Per-query wall/CPU time, whole-process and interpreter-adjusted RSS, and available
  buffer/spill metrics; compare both with C++ and with the preserved uncached baseline.
- Suite summaries plus separate query outlier tables. Repeat noisy results, investigate
  serious regressions, and state every unresolved limitation. Averages cannot hide
  serious speed or memory regressions or justify memory increases for minor speed gains.
- Confirmation that functional, file/WAL and memory-limit/out-of-core correctness gates
  remain intact, with raw evidence stored on the external SSD.

The owner has approved the baseline tradeoff; this document does not yet certify final
binary performance. The active name migration, client bindings and other work-queue
items must finish before final verification and acceptance can close the goal.
