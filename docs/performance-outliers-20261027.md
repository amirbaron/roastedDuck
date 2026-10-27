# Source-5 performance outlier follow-up

These measurements precede the encrypted-Parquet implementation. They establish a checked control checkpoint, not final artifact acceptance. All eight paired follow-up runs exited successfully; source, signatures, datasets and result checks were verified. Each engine retained three timed samples and one warm-up. Full raw samples are preserved in the external results files.

| Suite | Threads | Query | Rust / C++ ms | Speed C++/Rust | Rust / C++ RSS MB | Adjusted RSS ratio | Rust wall / uncached control | Rust RSS / control |
|---|---:|---|---:|---:|---:|---:|---:|---:|
| tpch10 | 1 | q03 | 416.105 / 332.543 | 0.799 | 162.6 / 792.2 | 0.188 | 1.040 | 1.011 |
| tpch10 | 1 | q18 | 1569.799 / 1421.895 | 0.906 | 1190.2 / 1654.2 | 0.717 | 1.016 | 1.003 |
| tpch10 | 1 | q21 | 1490.867 / 1242.190 | 0.833 | 546.9 / 991.1 | 0.544 | 0.989 | 1.007 |
| tpch10 | 14 | q05 | 76.647 / 59.082 | 0.771 | 173.6 / 817.7 | 0.196 | 1.025 | 1.023 |
| tpch10 | 14 | q08 | 63.461 / 51.583 | 0.813 | 162.8 / 1038.0 | 0.143 | 0.968 | 0.993 |
| tpch10 | 14 | q11 | 10.520 / 8.718 | 0.829 | 153.9 / 197.5 | 0.759 | 1.077 | 1.026 |
| tpch10 | 14 | q18 | 190.210 / 155.751 | 0.819 | 779.8 / 1484.0 | 0.520 | 0.951 | 1.032 |
| hits | 1 | q00 | 0.065 / 0.084 | 1.292 | 54.5 / 48.5 | 1.188 | 1.076 | 1.029 |
| hits | 1 | q19 | 25.569 / 10.006 | 0.391 | 60.6 / 265.6 | 0.178 | 1.004 | 1.016 |
| hits | 1 | q28 | 48245.048 / 32585.665 | 0.675 | 2251.1 / 4789.7 | 0.468 | 1.026 | 1.037 |
| hits | 1 | q32 | 8549.241 / 7463.918 | 0.873 | 12801.0 / 12082.0 | 1.060 | 1.092 | 0.976 |
| hits | 14 | q00 | 0.113 / 0.093 | 0.827 | 54.7 / 48.9 | 1.180 | 1.438 | 1.026 |
| hits | 14 | q19 | 5.057 / 2.033 | 0.402 | 71.6 / 268.1 | 0.219 | 1.190 | 1.022 |
| hits | 14 | q28 | 6146.391 / 3953.223 | 0.643 | 1932.1 / 5599.2 | 0.343 | 1.034 | 1.006 |
| hits | 14 | q29 | 23.348 / 17.017 | 0.729 | 74.8 / 231.3 | 0.272 | 0.941 | 1.017 |
| hits | 14 | q32 | 881.082 / 695.121 | 0.789 | 7853.9 / 9238.3 | 0.850 | 1.095 | 0.989 |

ClickBench q19 and q28 remain substantial baseline slowdowns and cannot be hidden by suite means. q28 takes 48.245 s versus C++ 32.586 s at one thread and 6.146 s versus 3.953 s at 14 threads, while using about 47% and 35% of whole-process memory. Against the uncached Rust control, its time increases 2.6%/3.4% and memory 3.7%/0.6%. q19 remains roughly 2.5 times C++ time, but its absolute cost is 25.6/5.1 ms and memory is markedly lower.

ClickBench q32 at one thread is a memory exception: Rust 12.801 GB versus C++ 12.082 GB (1.060 adjusted ratio). The preserved Rust control uses 13.111 GB; current Rust is 2.4% lower, while C++ varies across runs. This is not a general per-query memory win. At 14 threads q32 is 9.5% slower than the Rust control with 1.1% lower RSS; it still needs final-artifact follow-up. q00 adds about 6 MB whole-process memory versus C++ despite its small absolute footprint; microsecond timing ratios are sensitive to noise.

TPC-H q03/q05/q08/q18/q21 and tiny q11 retain the detailed ratios above. TPC-H q18 at one thread uses 1.190 GB versus control 1.187 GB; its changed C++ ratio is not evidence of a major Rust residency increase. These observations explain individual results, not new exceptions to the functionality goal.

The four complete source-5 configurations are 6.0%/13.5% longer for TPC-H one/14 threads with 77.5%/69.8% less adjusted RSS, and 12.1%/14.2% longer for ClickBench with 68.7%/62.7% less adjusted RSS. This fits the approved aggregate tradeoff. The listed outliers remain explicit, and final signed artifacts must be measured again after functional changes. No cache-residency optimization was introduced.

External evidence: `performance-acceptance-20261027/{initial-four-configurations.json,outlier-analysis.json,outlier-batch-terminal.json,uncached-outliers-*/results.json,repeat-outliers-*/results.json}`. Exact engine images and every timed sample are recorded in those reports.
