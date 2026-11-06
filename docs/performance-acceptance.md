# Performance acceptance

Policy approved by the owner on 2026-10-23. The preserved2026-10-29 signed native
checkpoint passes all130 TPC-H/ClickBench query/thread configurations and preserves
the accepted aggregate tradeoff. It predates the later Parquet, target-precision,
client and platform corrections. Its acceptance must not be attributed to newer
signed artifacts. The final source2551 refresh is now complete in the current acceptance section below. See
[the preserved native measurements and all query outliers](performance-final-native-20261029.md).
Weighted sums, maximum RSS and the63 individually reported exceptions remain explicit;
this is native performance acceptance, not whole-port completion. Source-5 results below
remain historical controls in [the earlier query report](performance-outliers-20261027.md).
This policy supersedes both the original outperformance targets and the later
independent speed/memory parity gates.

Preserve the measured lower-memory baseline. Roughly 15–20% longer execution time
with 60–70% less memory on TPC-H and ClickBench is acceptable. Do not increase memory
merely to match C++ caching or close a small speed gap. Evaluate both metrics and
report individual query outliers separately. Correctness, functional parity and
storage/WAL compatibility remain required.

## Current native candidate acceptance (2026-11-06)

Source2551/Mac2556 passed all 130 TPC-H SF10/ClickBench query/thread configurations, independent numeric recomputation and fresh full-result controls for all 13 tied configurations. No cache, memory budget, query, dataset or image policy changed. The [current report](performance-final-native-20261106.md) applies the historical geometric-mean definition and lists all 63 flagged queries, all 130 rows, four COPY profiles and five whole-file stress outliers.

Best-time geometric means are 7.2%/14.7% longer for TPC-H and 11.4%/15.6% longer for ClickBench. Adjusted geometric-mean RSS reductions are 78.1%/70.6% and 69.1%/62.2%, respectively. Raw whole-process RSS, absolute peaks, and best/median weighted sums remain visible. ClickBench's weighted sums are 22–25% longer; they do not meet a strict 20% cap. Its one-thread maximum RSS is 13.270GB C++ versus 13.116GB Rust, almost equal.

The existing aggregate policy and the macro condition attached to the approved 14-thread/256MB COPY exception are met. Single-thread COPY and all five whole-file performance limitations remain explicit; they are not individually waived or claimed to match C++. The [acceptance evidence](../.context/artifacts/platform-matrix-20261029/current-native-performance-acceptance-review-2919/verification.json) retains every disposition. Raw collector reports' original creation-time acceptance fields were not rewritten.

This performance acceptance changes no functional or artifact requirement. The owner separately accepted an explicit physical-device test exception on November 6, 2026; actual hardware execution/provisioning remains unverified. All other recorded software collection/review requirements are evidenced.

## Historical candidate measurement status (2026-11-03)

Corrected source2078/signed macOS arm64 candidate2088 supersedes source1959/1961 after the concurrent-index publication fix. No cache, buffer or query-memory policy changed. Its full SQL, selected-client, platform and storage/WAL verification is still in progress; all130 native TPC-H SF10/ClickBench configurations need fresh coupled speed/RSS measurements and individual-query dispositions on coherent corrected artifacts. Prepared2055/2066 and the source1959 client measurements below are historical preparation/evidence, and cannot grant acceptance for2078. The accepted lower-memory policy and documented memory-bound COPY exception remain unchanged.

The corrected preparation is now2173, with the measurement harness, query extraction, summary and outlier policy byte-identical to the preserved run. New functional gate2171 rejects missing current receipts before issuing a benchmark prerequisite. Independent review2174 retains exact integer comparisons, weighted best/median totals, maximum whole/adjusted RSS and every outlier. Additional2179 controls will record the full-result count/hash for each limited tied answer on the actual measured shared libraries after timing. Both benchmark entry points and2179 currently reject missing evidence before creating measurement output. These are verified preparation checks; no corrected-candidate macro measurements or performance acceptance exist yet.

Before final native macOS TPC-H/ClickBench timing, finish the required Linux work, verify that the
dedicated `duckdb` profile has no running containers, then stop that owned VM to
release its 24-GiB guest allocation. Preserve its data disk, stopped containers,
images and receipts. The existing workload guard accepts and records a verified
stopped profile. This isolates host memory pressure without changing either
engine's cache or memory policy; it is still a pending measurement prerequisite.

Earlier candidate measurement status (source1959/core1961):

Current source1959/signed macOS arm64 candidate1961 requires the actual native
130configuration TPC-H SF10/ClickBench refresh against authenticated C++934.
Prepared2055 preserves the original measurement harness, query extraction,
one warmup and three timed samples. Source gate1965 and the completed current
macOS functional prerequisite2054 are required before timing. The prerequisite
is not issued while required functional receipts are missing. Retired gate1660
and old candidate1637 do not grant acceptance for these artifacts.

All five current Mac Python cells and independent1980 reviews now complete under
matrix2024:108coupled client samples across18separate workloads, recorded in
[the binding report](python-fetch-conversion-parity.md). The current CP314 Python-
backed workload takes31.2%longer best time/29.3%longer median time with0.6%lower
maximum RSS; its Arrow-backed counterpart takes about21.6%of C++time with18.2%lower
maximum RSS. Every ABI/profile/storage workload remains separate. These client
observations do not establish macro acceptance.

Independent review2066 is prepared to recompute all130rows, weighted best/median
totals, geometric means, absolute whole-process and interpreter-adjusted maximum
RSS, and every named outlier from the original raw samples. It checks native/source
identities, all three samples and both original row/aggregate reports. It preserves
the measurement workload and writes no performance acceptance. Every flagged query
still requires a documented disposition; aggregate averages cannot close that gate.

The additional answer review keeps integer-formatted values exact: the original
float comparator accepts9007199254740992and9007199254740993as equal; the independent
review rejects this demonstrated counterexample. Decimal tolerance remains. Tied
limited results require a fresh recorded full-result count/hash control before
acceptance. Preparation2066 records the counterexample and rejects missing actual
measurements without creating an acceptance report. This exercises verification
code; native macro measurements remain unrun. Cache and query memory policy are
unchanged.

## Current SQL-file timing outliers (2026-11-03)

The corrected2078 full macOS collections retain55 individual file-time outliers. These five have the largest absolute Rust-minus-C++ file times. Both engines pass every listed file. Similar observations exist on the preceding1959candidate; this does not establish a regression caused by the index fix. Whole-file times include multiple statements, setup and finalization, and are separate from the130 TPC-H/ClickBench query configurations. Comparable peak RSS has not yet been measured for these replays, so no speed–memory acceptance is inferred.

| Original SQL file | C++ seconds | Rust seconds | Rust/C++ time |
|---|---:|---:|---:|
| VARIANT Parquet round-trip | 8.005 | 57.402 | 7.17 |
| Batch-insert space reclamation | 21.028 | 65.722 | 3.13 |
| Multiple large compressed CSV files | 63.911 | 95.550 | 1.50 |
| Concurrent attach/checkpoint | 1.413 | 25.217 | 17.85 |
| Concurrent checkpoint/index insertion | 6.258 | 29.471 | 4.71 |

All55 original rows and exact case paths remain in `.context/artifacts/platform-matrix-20261029/corrected-macos-full-corpus-review-2093/individual-file-time-outliers.json`. Prepared2188 retains all five complete original files and all assertions for three fresh samples per engine, measuring wall time, CPU and kernel peak RSS with unchanged16GiB engine budgets,20GiB safety guards and default thread policy. The original queue2189 stopped on the x86_64 corpus prerequisite; recovery dispatcher2222 schedules the unstarted outlier measurements through recovery2221 after diagnostics2220. No outlier replay has run yet. Original file timings and failures are never rewritten, and every serious outlier still needs a documented disposition. No cache or memory increase is proposed.

## Historical candidate measurement status (source1634/core1637)


Frozen source1634/signed macOS arm64 candidate1637 still requires fresh native
TPC-H SF10 and ClickBench measurements against authenticated C++934. Prepared1705
covers all130 query/thread configurations with checked answers, one warmup and
three timed samples. The benchmark, query extraction and reporting policy are
unchanged from1398; current source/artifact pins replace the historical candidate.
Gate1660 and an explicit current native-functional verification receipt must pass
before timing. Preparation1706 confirms that the current unreviewed state is
rejected before benchmark output or native execution. No current-candidate macro
performance acceptance has been granted.

Corrected macOS CP310 client measurements1758 are complete:four profile/storage
workloads and24 interleaved fresh-process samples retain coupled time/RSS and exact
native identities. Python-backed nullable-string scans take22–24% longer for about
1% lower whole-process RSS; Arrow-backed scans take about one fifth of the time
with25% lower RSS. All four rows and raw samples remain explicit in
[the binding report](python-fetch-conversion-parity.md). These client observations
do not substitute for the still-unrun TPC-H/ClickBench acceptance or other targets.

Corrected macOS CP311measurements1770and independent review1822now retain another
24coupled samples/four individual rows. Python-backed medians are1.207/1.208times
C++time at0.994/0.987whole-process RSS; Arrow-backed medians are0.201/0.186times
C++time at about0.756RSS. These are separate exact-ABI client observations, not
cross-ABI regression claims or macro acceptance. The same binding report preserves
every row, best/median time and whole/incremental/maximum RSS.

The report flags a query when its best **or median** Rust/C++ time exceeds1.25,
or adjusted **or whole-process** RSS exceeds0.6. Each flagged row names its reasons;
all absolute values, raw samples and query rows remain available. Both weighted
best and median time totals are reported. These are follow-up thresholds, not
independent speed/memory gates or automatic rejection of the accepted tradeoff.
Because reporting now includes median and whole-process criteria, outlier counts
alone must not be compared with the older63-case list as evidence of regressions.

Reporting-policy check1468 verifies that a median-only3x slowdown is retained even
when the best sample looks acceptable; it contains synthetic policy inputs only.
Isolation check1469 uses the actual live Linux build to verify that the native
benchmark refuses overlapping work inside this workspace's dedicated Docker VM.
Neither check executes benchmark queries or substitutes for real measurements.

## Accepted memory-bound COPY tradeoff

On 2026-10-29 the owner explicitly accepted the documented COPY memory-manager
throttling tradeoff, provided the full final TPC-H/ClickBench baseline remains
within the approved speed–memory range. The two-million-row calibration at14
threads/256MB is303.0ms/459.5MB peak RSS for corrected Rust versus246.6ms/463.8MB
for C++; the old unaccounted Rust path was87.4ms/665.0MB. All values are separate
three-sample medians, with decimal MB. The faster unaccounted path is not a
performance target to recover by increasing memory.

Retain individual query/configuration outliers and coupled time/RSS measurements.
This acceptance leaves functional/correctness, shell, persistence/WAL, C API/ADBC
and Python/Go/Swift gates intact. Final signed artifacts still need verification;
this policy decision does not complete the goal. See
[the COPY evidence](batch-copy-memory-parity.md) and external
`performance-policy-20261029/copy-acceptance/owner-decision.json`.

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

## C API result snapshot checkpoint (2026-10-25)

Fresh release-library C ABI samples retain 1000 checked trivial-query timings and
peak RSS together: 14.0–14.3 ms before versus 15.8–16.2 ms after, with warm-repeat
whole-process peaks 36.5–36.6 MB versus 36.8–37.2 MB. The first older-library sample
peaks at 45.3 MB and remains in the raw records. The older shipping library predates
other SDK integration changes; do not attribute the entire difference to the new
Arrow settings snapshot. This bounded client microcheck does not replace final
TPC-H/ClickBench or per-query acceptance. No table caching is introduced.
Evidence: `.context/artifacts/capi-arrow-snapshot-20261025/performance-comparison.json`.

## Python catalog metadata lookup checkpoint (2026-10-25)

Matching C++'s physical-table/view fallback adds a metadata lookup. Two paired fresh
development repeats expose the cost: 1000 physical-table operations increase from
8.5–8.9 to 10.9–11.2 ms; registered-table operations from 22–23 to 27–28 ms. Direct
views remain about 22–23 ms and registration about 100–105 ms. Whole-process RSS
stays around 118–119 MB; first cold 142/139 MB samples are retained. Reader prefixes
remain around 73 MB for 1M/100M source rows. Preserve these individual lookup costs;
they are not hidden in an aggregate or accepted as final engine performance. No
table cache was added to recover the extra few microseconds per lookup.
Evidence: `.context/artifacts/python-catalog-names-20261025/performance-before-after.json`.

## Python string-scan outlier (2026-10-25)

The nullable one-million-row Pandas string scan remains an individual open outlier.
After direct native-chunk UTF-8 copies and singleton NULL checks, Rust development
repeats are 65.2–66.1 ms for Python extension strings versus C++ release 35.2–35.4 ms,
and 114.2–115.0 ms for Arrow extension strings versus 57.5–57.7 ms. Peak RSS is
143.8–144.2 MB versus 141.3–142.3 MB, and 215.3–215.5 MB versus about 197.7 MB.
All count/length checks pass. Earlier Rust times were 136.7–139.7/186.8–193.0 ms;
the speed improvement does not materially increase memory or introduce a cache.

The remaining roughly 1.9×/2.0× slowdown needs release-build investigation and explicit
reporting. It is not accepted by the separate TPC-H/ClickBench baseline policy.
Before/intermediate/final samples, controls and result checks are retained in
`.context/artifacts/python-pandas-remaining-20261025/performance*-comparison.json`.

The current signed release wheel repeats this outlier on2026-10-29: three paired
samples give35.09/60.71ms C++/Rust for Python strings (142.11/139.46MB) and
58.82/112.97ms for Arrow strings (198.02/212.43MB). This confirms1.73x/1.92x time
ratios in release builds. Raw samples and whole-process RSS are retained; the older
development-versus-release explanation is no longer sufficient. See
[final client evidence](final-client-verification-20261029.md).

## Python execution/reader checkpoint (2026-10-25)

After native signal polling and reader lifecycle corrections, the final signed
development wheel's 1000-operation samples are 34.2–34.8 ms/~47 MB for insert-many
versus C++ 233–241 ms/~117 MB, and 73–78 ms/~46 MB for parsed/text execution versus
227–243 ms/~115 MB. First-parameter initialization is included as documented by the
measurement driver. Earlier noisier insert samples, Rust 50.7–67.1 ms and C++ 384–506 ms,
are preserved separately; no performance improvement is attributed to the later
Explain-only changes. These client samples do not prove engine outperformance.

Reading only the first 1024 rows of logical 1M/100M-row queries stays bounded at
about 73 MB versus C++ 71 MB, and connection reuse succeeds afterward. The per-case
latency difference remains visible: Rust 0.41–0.53 ms versus C++ 0.25–0.34 ms. It adds
about 0.1–0.2 ms; final release acceptance must assess both its absolute magnitude and
the bounded memory behavior. It is not silently averaged into the suite baseline.
All checksums, individual samples and earlier rounds are retained in
`.context/artifacts/python-arrow-remaining-20261025/performance*-comparison.json`.
The uncached baseline and cache policy remain unchanged; these are not final release
measurements or a new exact-speed requirement.

## Python type/callback context checkpoint (2026-10-25)

Read-only callback metadata survives planning and streamed batches, and is released at
query end; it retains catalog/transaction state rather than table blocks. An initial
build also initialized the default database eagerly through DB-API built-in type
constants. Its roughly 4 MB RSS increase was rejected and the bootstrap now uses native
built-in constants directly. Normal user type parsing remains catalog-aware.

Two-repeat fresh-process comparisons against the preserved signed pre-change wheel,
with the same interpreter and test dependencies, record 1000 insert-many operations at
35.2–35.5 ms versus 32.9–33.7 ms before (~6% longer), and parsed/text loops at
70.5–72.9 ms versus 69.8–73.4 ms. Whole-process peaks remain about 45–49 MB versus
46–48 MB. First1024-row reads from logical1M/100M sources stay near72 MB versus73 MB,
with source-size independence and connection recovery verified. All checksums pass;
the rejected samples remain preserved. These are development measurements, not final
release acceptance. No result/input/table cache was added, and the accepted uncached
TPC-H/ClickBench baseline remains the control.

Evidence: `.context/artifacts/python-type-api-20261025/performance-summary-7.json`,
raw `performance-*-before-after-7.json`, staged memory probes and prior rejected round.

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

## Report tooling verification

The benchmark report now presents the approved policy and separate slowest/highest-RSS
query tables. The old 1.3×/0.6× gates are removed; comparisons require the preserved
Rust-baseline ratchet and documented acceptance. Result-mismatch and missing-result
reporting remain intact. Four report tests and the per-query regression-ratchet test
pass in a standalone build of the exact repository modules (no engine implementation
is substituted). This isolated check is necessary while the workspace's name migration
is unfinished; the full workspace and final performance runs remain pending. Test logs,
verifier sources and dependency lockfile are retained under
`.context/artifacts/performance-policy-20261023/`.


## Current source-5 measured checkpoint (2026-10-27)

The strictly signed shipping library `38603874…` completes all 130 query/thread
configurations below with checked results: TPC-H SF10's 22 queries and all 43 ClickBench
queries, each at one/14 threads. Each engine/query runs in its own process with one
checked warmup and three timed repetitions. The original harness's best-wall/CPU and
peak-RSS definitions are retained; all timing samples and result dumps are preserved.
The external harness adaptation pins the signed CLI used for tie checks, verifies the
actual C ABI image with dladdr, and records samples. Library hashing stays outside the
measured child to avoid inflating its RSS. Source, image/signature and dataset hashes
verify unchanged before/after each configuration.

| Suite | Threads | C++ time / Rust time | Rust execution longer | Adjusted Rust/C++ RSS | Less adjusted RSS |
|---|---:|---:|---:|---:|---:|
| TPC-H SF10 | 1 | 0.943× | 6.0% | 0.225× | 77.5% |
| TPC-H SF10 | 14 | 0.881× | 13.5% | 0.302× | 69.8% |
| ClickBench | 1 | 0.892× | 12.1% | 0.313× | 68.7% |
| ClickBench | 14 | 0.876× | 14.2% | 0.373× | 62.7% |

These aggregate geometric means satisfy the owner's policy. They do not hide individual
outliers: ClickBench q19 is about 0.37–0.41× speed; q28 at one thread is 46.9 s versus
31.4 s; q00 adds roughly 6 MB whole-process RSS at 14 threads; q32 at one thread peaks
at 13.1 GB versus C++ 12.5 GB. Historical q32 Rust RSS was also 13.1 GB while C++ was
14.1 GB, so both raw bytes and fresh same-host control repeats are required before
attributing the ratio change. Notable TPC-H timing/RSS changes are also selected for
paired current/uncached-control repeats. Those repeats are currently active.

Evidence: external `performance-acceptance-20261027/initial-four-configurations.json`,
per-configuration manifests/results/dumps/verification, and `outlier-followups.json`.
All datasets and output are on the external SSD; workloads are serialized. No table
cache or memory-retention optimization was introduced.

This is a source-5 performance checkpoint, not final goal completion. The completion
audit found a real Parquet footer-key encrypted-writing gap still returning
NotImplemented in Rust. C++ 1.5.6 supports that path; its `column_keys` rejection is a
shared limitation and does not require implementing more than C++. Functional gaps must
be closed, final signed artifacts reverified, and outlier evidence retained before the
final release acceptance is claimed.


Current source1959 concurrency-file observation (2026-11-03):
`concurrent_checkpoint_deletes_mix_index.test_slow` passes on both engines.
Rust took 17.296 s versus C++ 8.858 s (1.953 times). Prior Rust took 17.125 s,
so the current Rust observation is about 1.0% longer. At 0.5-second sampling,
the worker peak RSS observations are 122,945,536 bytes for Rust and
443,727,872 bytes for C++ (Rust ratio 0.277, about 72.3% lower). This single
concurrency-file observation remains an individual outlier; sampled RSS can miss
short peaks, and these file timings are not TPC-H/ClickBench acceptance. No cache,
buffer or memory-budget change was made in response. The complete fresh corpus
review retains every file-time outlier. Evidence:
`.context/artifacts/platform-matrix-20261029/current-parallel-checkpoint-timing-observation-2010.json`.


## Corrected Linux ARM64 SQL-file observations (2026-11-03)

Both complete current ARM64 collections2131are reviewed in2212:Cpp4,842passes28skips, Rust4,828passes28skips14documentedraw failures, zero unknown nonpasses/skip differences/OOM/resource kills. All41individual whole-file time outliers are retained separately. These are original multi-statement test-file observations on the dedicated2CPUVM, not isolated query benchmarks or native TPC-H/ClickBench acceptance. Comparable whole-file peakRSS rechecks are still pending; no speed–memory acceptance is inferred.

| Original SQL file | C++ seconds | Rust seconds | Rust/C++ time |
|---|---:|---:|---:|
| `test/parquet/variant/variant_roundtrip.test_slow` | 14.593 | 113.687 | 7.79 |
| `test/sql/storage/parallel/reclaim_space_batch_insert.test_slow` | 26.849 | 79.102 | 2.95 |
| `test/sql/copy/csv/test_multiple_big_compressed_csvs.test_slow` | 74.719 | 113.989 | 1.53 |
| `test/sql/storage/parallel/reclaim_space_primary_key_optimistic.test_slow` | 15.637 | 39.546 | 2.53 |
| `test/db-benchmark/join.test_slow` | 23.594 | 42.335 | 1.79 |

Every original row remains in `.context/artifacts/platform-matrix-20261029/corrected-Linux-ARM-full-SQL-review-2212/individual-file-time-outliers.json`; all9,740file observations and70nonpasses remain alongside it. The largest VARIANT/write/reclamation observations reinforce the need for the queued2188time/kernelRSS replays and individual dispositions; native130macro acceptance still requires actual measurements and every query outlier. Source2078/uncached baseline is unchanged.
