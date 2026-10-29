# Performance acceptance

Policy approved by the owner on 2026-10-23. The current signed native candidate now
passes all130 TPC-H/ClickBench query/thread configurations and preserves the accepted
aggregate tradeoff. See [final native measurements and all query outliers](performance-final-native-20261029.md).
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
