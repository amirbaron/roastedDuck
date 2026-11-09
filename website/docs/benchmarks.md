# Benchmarks: inspect the tradeoff

The measured candidate uses less memory on many workloads, with higher execution
times on others. This page keeps the aggregate, every query, and the unfavorable
cases together.

## Recorded comparison

Measurements were collected on **macOS arm64**, with one warmup and three timed
repetitions per engine/query configuration. TPC-H uses scale factor 10; ClickBench
uses the retained `hits` database. There are 65 queries at 1 and 14 threads:
**130 configurations**. These are engineering workloads, not audited TPC results.

The local host was re-identified during public packaging as an Apple M4 Pro with
48 GiB RAM, running macOS 27.0.1. That identification is dated November 8; the
measurement receipts are dated November 5 and the acceptance report November 6.
No benchmark rerun was performed for the public branding changes.

| Suite / threads | Best-time geometric mean, Rust / C++ | Whole-process RSS geometric mean, Rust / C++ | Summed best time, Rust / C++ |
| --- | --- | --- | --- |
| TPC-H SF10 / 1 | 1.072 | 0.245 | 1.003 |
| TPC-H SF10 / 14 | 1.147 | 0.313 | 1.110 |
| ClickBench / 1 | 1.114 | 0.335 | 1.229 |
| ClickBench / 14 | 1.156 | 0.399 | 1.254 |

Time ratios above 1 mean slower. RSS ratios below 1 mean lower memory. The
memory figure is a geometric mean of per-query process peaks, **not a guarantee
about the largest query or total process footprint**. Whole-process RSS includes
the Python measurement driver; interpreter-adjusted values are retained in the
downloadable data, separately from the whole-process headline.

<div class="rd-benchmark" id="benchmark-explorer" data-source="../evidence/benchmark-results.json">
  <div class="rd-benchmark-controls">
    <label for="benchmark-suite">Workload</label>
    <select id="benchmark-suite"><option value="tpch10-1">TPC-H SF10 · 1 thread</option><option value="tpch10-14">TPC-H SF10 · 14 threads</option><option value="hits-1">ClickBench · 1 thread</option><option value="hits-14">ClickBench · 14 threads</option></select>
    <label for="benchmark-metric">Metric</label>
    <select id="benchmark-metric"><option value="memory">Peak RSS geometric mean</option><option value="time">Best-time geometric mean</option><option value="total">Summed best time</option></select>
  </div>
  <div aria-live="polite" id="benchmark-output">Loading recorded measurements. The table above and downloads below remain available without JavaScript.</div>
</div>

## Cases the headline does not describe

- ClickBench summed best times are **22.9% / 25.4% longer** at 1 / 14 threads.
  They do not satisfy a strict 20% total-time cap.
- The largest single-thread ClickBench peak is **13.270 GB C++ / 13.116 GB Rust**:
  almost no saving. At 14 threads the maxima are 11.890 / 7.970 GB.
- Single-thread COPY is **37.5% slower with 46.0% higher median RSS**.
- Separate whole-file stress cases retain these time / RSS ratios:
  VARIANT **5.391× / 0.376×**, batch reclaim **3.115× / 0.774×**, compressed CSV
  **1.494× / 1.102×**, checkpoint/index **3.042× / 0.593×**, and primary-key
  reclaim **2.509× / 0.977×**.
- Failed reference runs do not count as successful timing comparisons.

All 63 flagged configurations remain in the original summary. The dated
[full performance report](https://github.com/amirbaron/roastedDuck/blob/main/docs/performance-final-native-20261106.md)
contains each disposition and the separate COPY/stress profiles.

## Download the evidence

- [Every query and all timed samples, JSON](evidence/benchmark-results.json)
- [All 130 configurations, CSV](evidence/all130-queries.csv)
- [Dataset and engine identities, JSON](evidence/benchmark-provenance.json)

The original summary is copied byte-for-byte, including its historical
acceptance fields. Later scoped acceptance is explained in the dated report;
raw measurements and failed observations were not rewritten. The JSON contains
best and median times, CPU observations, process peaks, individual samples, and
outlier reasons. Dataset hashes identify the exact files used; the multi-GB
datasets are not included in the documentation repository.

## Reproduce and compare

The repository's `scripts/perf/bench.py`, `data.sh`, and `suites.sh` provide a
local comparison harness. Supply the pinned DuckDB 1.5.6 reference source and
build, and point the environment at your data and Rust library:

```sh
export DUCKDB_SRC=/absolute/path/to/duckdb-1.5.6
export ROASTEDDUCK_PERF_DIR=/absolute/path/to/benchmark-data
export RUST_LIB=/absolute/path/to/roastedduck/lib/libduckdb.dylib

# Prepare your TPC-H dataset with the reference engine.
scripts/perf/data.sh tpch 10
python3 scripts/perf/bench.py tpch10 --threads 1 --reps 3 --out tpch10-t1.json
python3 scripts/perf/bench.py tpch10 --threads 14 --reps 3 --out tpch10-t14.json
```

The harness defaults to macOS library paths; porting the harness or switching
platforms produces a new experiment. For ClickBench, `data.sh hits N` selects
how many source partitions to load. Do not compare a different dataset to the
published headline as though it were the same experiment. Verify file hashes
against the provenance record to establish exact input equality.

For a new result, publish machine/OS/toolchain details, dataset size and hashes,
query inputs, thread/memory settings, warmups, every timed sample, correctness
checks, and individual outliers. Never substitute a failed reference run's
duration for a successful baseline. Native Linux and physical iOS performance
are not established by these measurements.
