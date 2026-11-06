# Signed native performance acceptance — 2026-10-29

The current native candidate meets the owner-approved aggregate speed–memory policy.
The whole port goal remains active. This review uses the historical geometric-mean
definition, retains weighted sums and whole-process peaks, and reports every query.
It does not require exact C++ performance or authorize increased table caching.

The immutable library is `01aaeb24d068580f7755656062ba7a3e8a31bbbdfd4a1c5933379a59ecb0a702`.
All 130 query/thread configurations (22 TPC-H SF10 and 43 ClickBench, each one/14
threads) pass checked answers. Each engine/query has one warmup and three retained
timed samples. All source/query/dataset hashes, selected engine images and strict
signatures verify before/after; the batch parent exits zero. No source edits or
other build/test workloads overlap these measurements.

| Suite | Threads | Longer execution, geometric mean | Less RAM, adjusted geometric mean | Longer summed time | C++ / Rust maximum RSS, GB |
|---|---:|---:|---:|---:|---:|
| TPC-H SF10 | 1 | 7.3% | 77.9% | 0.8% | 2.39 / 1.19 |
| TPC-H SF10 | 14 | 17.3% | 69.8% | 14.8% | 2.41 / 1.22 |
| ClickBench | 1 | 11.6% | 68.3% | 22.7% | 11.18 / 11.61 |
| ClickBench | 14 | 14.7% | 62.8% | 22.2% | 12.06 / 7.86 |

Memory ratios above subtract the measured Python-interpreter baseline from each
process peak, matching the preserved acceptance baseline. Raw RSS is retained too.
Maximum RSS values may come from different queries; they are not geometric means.
Median-time geometric means are also retained and range from 7.1–18.6% longer.

The owner accepted roughly15–20% longer execution with60–70% less memory. These
geometric means preserve or improve that tradeoff. ClickBench weighted sums are
22.7%/22.2% longer; this is explicit, not a claim that totals meet a strict20% cap.
The preserved accepted uncached baseline already had23.3%/28.7% longer ClickBench
sums. Current summed ratios improve that baseline. The small one-thread TPC-H
sum increase is0.8%;14-thread TPC-H is14.8%.

## Individual outliers

All63 configurations with Rust/C++ time >1.25 or adjusted RSS >0.6 are listed below.
These reporting thresholds identify exceptions; they do not impose new independent
per-query speed/RAM targets. Absolute sizes/times distinguish tiny queries from
large workloads. All130 rows and all timed samples remain in the external JSON.

| Suite | Threads | Query | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB | Adjusted RAM ratio |
|---|---:|---|---:|---:|---:|---:|---:|---:|
| tpch10 | 1 | q02 | 76.76 | 91.22 | 1.19 | 157.47 | 114.82 | 0.70 |
| tpch10 | 1 | q03 | 325.30 | 410.18 | 1.26 | 783.30 | 162.05 | 0.19 |
| tpch10 | 1 | q11 | 46.09 | 51.01 | 1.11 | 151.42 | 98.03 | 0.61 |
| tpch10 | 1 | q16 | 145.62 | 170.17 | 1.17 | 304.64 | 210.83 | 0.67 |
| tpch10 | 14 | q02 | 22.20 | 27.35 | 1.23 | 267.01 | 198.51 | 0.73 |
| tpch10 | 14 | q03 | 51.82 | 67.06 | 1.29 | 848.35 | 214.35 | 0.24 |
| tpch10 | 14 | q05 | 52.60 | 80.30 | 1.53 | 812.91 | 183.04 | 0.21 |
| tpch10 | 14 | q07 | 41.41 | 58.44 | 1.41 | 1020.64 | 285.98 | 0.27 |
| tpch10 | 14 | q08 | 44.78 | 57.02 | 1.27 | 1035.01 | 166.12 | 0.15 |
| tpch10 | 14 | q11 | 8.05 | 10.08 | 1.25 | 198.26 | 152.60 | 0.75 |
| tpch10 | 14 | q12 | 48.30 | 64.02 | 1.33 | 681.18 | 133.19 | 0.18 |
| tpch10 | 14 | q16 | 37.32 | 41.79 | 1.12 | 598.98 | 367.67 | 0.60 |
| tpch10 | 14 | q20 | 42.16 | 53.43 | 1.27 | 882.29 | 155.71 | 0.16 |
| tpch10 | 14 | q21 | 176.21 | 265.71 | 1.51 | 1096.53 | 633.00 | 0.57 |
| tpch10 | 14 | q22 | 38.99 | 40.28 | 1.03 | 210.68 | 144.61 | 0.66 |
| hits | 1 | q00 | 0.09 | 0.07 | 0.84 | 48.86 | 54.23 | 1.17 |
| hits | 1 | q01 | 10.58 | 14.59 | 1.38 | 169.03 | 59.29 | 0.28 |
| hits | 1 | q02 | 141.07 | 183.37 | 1.30 | 319.34 | 62.08 | 0.15 |
| hits | 1 | q05 | 1776.43 | 2493.23 | 1.40 | 1847.75 | 768.54 | 0.41 |
| hits | 1 | q06 | 0.48 | 0.27 | 0.56 | 69.99 | 56.39 | 0.75 |
| hits | 1 | q07 | 16.65 | 21.46 | 1.29 | 170.43 | 60.29 | 0.29 |
| hits | 1 | q10 | 430.30 | 561.50 | 1.30 | 616.28 | 127.11 | 0.18 |
| hits | 1 | q15 | 1689.18 | 1985.61 | 1.18 | 1571.50 | 1042.43 | 0.66 |
| hits | 1 | q16 | 3509.31 | 4137.76 | 1.18 | 4563.09 | 3157.80 | 0.69 |
| hits | 1 | q17 | 3375.89 | 4124.55 | 1.22 | 3937.68 | 3156.62 | 0.80 |
| hits | 1 | q18 | 5626.67 | 6012.65 | 1.07 | 7870.60 | 6476.79 | 0.82 |
| hits | 1 | q19 | 9.35 | 26.14 | 2.80 | 265.54 | 60.19 | 0.18 |
| hits | 1 | q28 | 31989.47 | 46954.59 | 1.47 | 4741.05 | 2294.37 | 0.48 |
| hits | 1 | q29 | 126.08 | 157.97 | 1.25 | 230.20 | 64.41 | 0.22 |
| hits | 1 | q32 | 7856.69 | 8211.51 | 1.05 | 11184.98 | 11608.97 | 1.04 |
| hits | 1 | q35 | 1745.15 | 1994.16 | 1.14 | 1068.24 | 748.55 | 0.70 |
| hits | 1 | q37 | 8.89 | 21.44 | 2.41 | 97.06 | 68.06 | 0.64 |
| hits | 1 | q39 | 87.68 | 96.06 | 1.10 | 295.45 | 220.73 | 0.73 |
| hits | 1 | q40 | 3.38 | 4.81 | 1.42 | 93.18 | 64.55 | 0.63 |
| hits | 1 | q41 | 4.35 | 5.54 | 1.28 | 89.74 | 63.86 | 0.65 |
| hits | 1 | q42 | 9.72 | 8.89 | 0.92 | 82.15 | 62.59 | 0.70 |
| hits | 14 | q00 | 0.09 | 0.08 | 0.87 | 48.81 | 54.64 | 1.18 |
| hits | 14 | q01 | 2.18 | 2.90 | 1.33 | 171.23 | 67.93 | 0.33 |
| hits | 14 | q02 | 15.09 | 24.63 | 1.63 | 320.36 | 71.50 | 0.18 |
| hits | 14 | q04 | 138.84 | 181.22 | 1.31 | 1024.74 | 526.17 | 0.51 |
| hits | 14 | q05 | 189.88 | 257.33 | 1.36 | 2078.41 | 1027.39 | 0.49 |
| hits | 14 | q06 | 0.53 | 0.21 | 0.40 | 69.98 | 56.48 | 0.75 |
| hits | 14 | q07 | 2.77 | 4.01 | 1.45 | 173.97 | 70.17 | 0.34 |
| hits | 14 | q08 | 154.75 | 201.17 | 1.30 | 1203.90 | 710.95 | 0.58 |
| hits | 14 | q10 | 49.14 | 69.01 | 1.40 | 849.07 | 268.11 | 0.30 |
| hits | 14 | q11 | 54.17 | 71.71 | 1.32 | 892.17 | 285.21 | 0.31 |
| hits | 14 | q12 | 170.91 | 223.24 | 1.31 | 2139.42 | 1079.26 | 0.50 |
| hits | 14 | q16 | 375.12 | 467.09 | 1.25 | 3671.93 | 2383.05 | 0.65 |
| hits | 14 | q17 | 298.87 | 388.89 | 1.30 | 3844.34 | 2403.24 | 0.62 |
| hits | 14 | q18 | 640.26 | 740.22 | 1.16 | 6867.30 | 4146.09 | 0.60 |
| hits | 14 | q19 | 1.60 | 3.98 | 2.49 | 268.40 | 71.61 | 0.22 |
| hits | 14 | q25 | 86.49 | 109.67 | 1.27 | 581.42 | 84.51 | 0.12 |
| hits | 14 | q27 | 184.99 | 234.10 | 1.27 | 4998.61 | 307.40 | 0.06 |
| hits | 14 | q28 | 3839.75 | 5396.00 | 1.41 | 5573.31 | 1969.55 | 0.35 |
| hits | 14 | q29 | 15.89 | 25.43 | 1.60 | 233.00 | 74.84 | 0.27 |
| hits | 14 | q32 | 646.81 | 774.90 | 1.20 | 9383.90 | 7862.19 | 0.84 |
| hits | 14 | q36 | 12.85 | 12.92 | 1.01 | 254.92 | 181.78 | 0.69 |
| hits | 14 | q37 | 3.16 | 5.71 | 1.81 | 133.02 | 98.73 | 0.71 |
| hits | 14 | q38 | 3.79 | 4.79 | 1.26 | 152.85 | 101.91 | 0.63 |
| hits | 14 | q39 | 20.55 | 24.24 | 1.18 | 433.36 | 286.31 | 0.65 |
| hits | 14 | q40 | 1.90 | 2.08 | 1.10 | 117.11 | 90.91 | 0.74 |
| hits | 14 | q41 | 1.88 | 2.15 | 1.14 | 106.66 | 92.72 | 0.85 |
| hits | 14 | q42 | 2.46 | 2.81 | 1.14 | 100.14 | 82.58 | 0.79 |

ClickBench q19 retains the known2.80x/2.49x time outlier, with about73–77% lower
whole-process RSS. q37 is2.41x/1.81x, an extra12.5/2.6ms. q28 is46.95s versus31.99s
at one thread, with2.29GB versus4.74GB RSS; the preserved uncached paired control
was47.01s versus31.82s. At14 threads q28 is5.40s versus3.84s with1.97GB versus5.57GB.
q32 at one thread is the memory exception:11.61GB versus11.19GB, roughly4% higher,
rather than the suite-wide reduction. The previous accepted baseline also showed
higher Rust memory for this query. q00 adds about5.4/5.8MB while timing near0.1ms.

Fresh paired preserved-uncached/current14-thread TPC-H repeats retain q05/q21:
current Rust64.39/222.72ms versus uncached66.36/222.45ms. Current RSS167.12/636.58MB
versus166.54/635.39MB. The first full run’s80.31/265.69ms remain preserved; the larger
slowdown does not reproduce as a current-vs-baseline regression. Each repeat also
runs the same C++ controls and checks answers, signatures, source and dataset hashes.

## Scope and evidence

This satisfies the full macro-baseline condition attached to the owner’s explicit
COPY tradeoff acceptance. The COPY303ms/459MB versus C++247ms/464MB calibration
remains a separate outlier. It does not waive functional/correctness/storage/client
gates or the separately measured Python string-scan outliers.

Evidence: `.context/artifacts/final-verification-20261029/performance-1/` contains
`batch-terminal.json`, four per-configuration manifests/results/verifications,
`summary.json`, `acceptance-review.json`, all checked result dumps, raw logs, all
samples and both outlier-repeat receipts. Its external storage root is
`/Volumes/amirs ssd/ai context/conductor/duckdb-rust/ottawa-50198a764d3b1fdc/`.

The initial summary script had a Python spacing syntax error. The original script
and error log are retained; correcting the report generator neither reran nor
changed benchmark samples/results.
