# Current native speed–memory acceptance — 2026-11-06

The current source2551/Mac2556 candidate satisfies the existing owner-approved **aggregate macro** speed–memory policy, using the historical geometric-mean definition. No exact C++ parity, outperformance, extra table caching or larger query-memory budget is required. This is scoped performance acceptance, not whole-port or physical-iOS acceptance.

All130 query/thread configurations completed one warmup and three retained timed samples per engine. Independent review2743 recomputed every row and rechecked117 non-tied dumps; fresh full-result count/hash controls2744 cover the13 tied configurations/nine distinct queries. Source/query/dataset/native image/signature guards remained stable. All63 flagged query configurations are reviewed individually below; all130 rows are in `all130-query-time-memory.csv`.

Ratios below are Rust/C++. Time >1 means slower; RSS <1 means less memory. RSS peaks come from the whole process; adjusted RSS subtracts the measured interpreter baseline. The memory headline is a geometric mean across queries, **not the maximum footprint**. Maximums may come from different queries.

| Suite | Threads | GM time best / median | GM whole / adjusted RSS | Summed time best / median | C++ / Rust max whole RSS GB |
|---|---:|---:|---:|---:|---:|
|TPC-H SF10|1|1.072 / 1.067|0.245 / 0.219|1.003 / 1.001|2.818 / 1.193|
|TPC-H SF10|14|1.147 / 1.135|0.313 / 0.294|1.110 / 1.102|2.385 / 1.215|
|ClickBench|1|1.114 / 1.112|0.335 / 0.309|1.229 / 1.216|13.270 / 13.116|
|ClickBench|14|1.156 / 1.142|0.399 / 0.378|1.254 / 1.227|11.890 / 7.970|

Best-time geometric means are7.2%/14.7% longer for TPC-H and11.4%/15.6% longer for ClickBench. Interpreter-adjusted memory reductions are78.1%/70.6% and69.1%/62.2%, respectively; raw whole-process geometric-mean reductions are75.5%/68.7% and66.5%/60.1%. These preserve the accepted lower-memory tradeoff.

ClickBench summed best times are22.9%/25.4% longer; summed medians are21.6%/22.7% longer. **These totals do not meet a strict20% cap.** The historical accepted report also retained summed-time overruns and used geometric means as its existing definition; the metric has not been changed for this candidate. Both best and median totals remain visible.

ClickBench q28 is the largest absolute slowdown: at one thread32.722s C++ versus47.158s Rust median (1.441x), with4.883GB versus2.098GB peak RSS. At14 threads it is3.591s versus5.014s (1.396x),5.611GB versus1.961GB RSS. ClickBench q32 at one thread uses13.270GB versus13.116GB RSS (0.988x), so it receives almost none of the suite-level memory saving. q00 adds about8MB RSS while executing near0.1ms. These are capacity/per-query limitations, not hidden aggregate wins.

## Every flagged native query configuration

The unchanged reporting flags identify time >1.25x or RSS >0.6x, rather than independent per-query acceptance targets. Absolute costs and all recorded samples remain. No raw result or timed sample was replaced by the later tied-result controls.

| Suite | Threads | Query | C++ / Rust median ms | Time best / median ratio | C++ / Rust whole RSS MB | Whole / adjusted RSS ratio | Individual review |
|---|---:|---|---:|---:|---:|---:|---|
|tpch10|1|q02|75.82 / 87.23|1.151 / 1.150|158.17 / 103.91|0.657 / 0.617|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|tpch10|1|q11|46.27 / 51.24|1.110 / 1.107|144.82 / 98.21|0.678 / 0.637|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|tpch10|1|q18|1500.35 / 1587.57|1.073 / 1.058|1973.99 / 1192.57|0.604 / 0.601|Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|tpch10|1|q22|271.55 / 287.15|1.070 / 1.057|166.28 / 102.38|0.616 / 0.574|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|tpch10|14|q02|20.56 / 26.94|1.352 / 1.310|271.91 / 161.19|0.593 / 0.567|Relative time flag with at most20ms absolute delta; retained separately.|
|tpch10|14|q03|54.53 / 69.13|1.295 / 1.268|862.67 / 215.04|0.249 / 0.235|Relative time flag with at most20ms absolute delta; retained separately.|
|tpch10|14|q07|43.48 / 58.32|1.362 / 1.341|1026.79 / 284.26|0.277 / 0.265|Relative time flag with at most20ms absolute delta; retained separately.|
|tpch10|14|q08|44.82 / 57.75|1.287 / 1.289|1028.96 / 148.13|0.144 / 0.130|Relative time flag with at most20ms absolute delta; retained separately.|
|tpch10|14|q10|93.15 / 116.34|1.268 / 1.249|1289.50 / 559.12|0.434 / 0.426|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|tpch10|14|q11|8.20 / 9.18|1.150 / 1.120|196.00 / 155.98|0.796 / 0.777|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|tpch10|14|q16|39.27 / 44.66|1.250 / 1.137|592.99 / 361.66|0.610 / 0.599|Relative time flag with at most20ms absolute delta; retained separately. Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|tpch10|14|q17|47.07 / 62.00|1.313 / 1.317|710.00 / 126.14|0.178 / 0.158|Relative time flag with at most20ms absolute delta; retained separately.|
|tpch10|14|q22|35.28 / 35.49|1.003 / 1.006|205.60 / 136.28|0.663 / 0.634|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|1|q00|0.10 / 0.08|0.855 / 0.801|48.81 / 56.85|1.165 / 1.247|Higher whole-process RSS; peak regression remains visible.|
|hits|1|q01|9.81 / 14.59|1.480 / 1.488|168.95 / 61.67|0.365 / 0.297|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|1|q02|142.77 / 179.29|1.258 / 1.256|319.24 / 64.05|0.201 / 0.158|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|1|q05|1741.25 / 2507.91|1.456 / 1.440|1956.15 / 771.46|0.394 / 0.389|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|1|q06|0.47 / 0.21|0.435 / 0.458|69.94 / 58.69|0.839 / 0.790|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|1|q07|15.87 / 20.87|1.319 / 1.315|170.05 / 62.47|0.367 / 0.300|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|1|q10|423.47 / 538.58|1.268 / 1.272|634.42 / 129.02|0.203 / 0.182|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|1|q15|1747.76 / 2009.57|1.155 / 1.150|1712.47 / 1045.81|0.611 / 0.607|Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|hits|1|q16|3708.72 / 4183.20|1.128 / 1.128|4169.01 / 3161.34|0.758 / 0.757|Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|hits|1|q17|3492.85 / 4187.57|1.177 / 1.199|5108.51 / 3159.20|0.618 / 0.617|Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|hits|1|q18|5795.83 / 6241.04|1.097 / 1.077|8767.88 / 6478.09|0.739 / 0.738|Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|hits|1|q19|9.36 / 25.93|2.755 / 2.771|265.62 / 62.64|0.236 / 0.186|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|1|q22|2328.92 / 3068.05|1.159 / 1.317|7311.62 / 130.55|0.018 / 0.016|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|1|q25|817.63 / 1004.72|1.251 / 1.229|568.12 / 66.24|0.117 / 0.091|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|1|q28|32721.91 / 47157.52|1.455 / 1.441|4882.96 / 2098.41|0.430 / 0.428|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee. Large absolute peak; capacity planning must use this row.|
|hits|1|q29|125.14 / 163.00|1.321 / 1.303|230.00 / 63.88|0.278 / 0.223|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|1|q32|8090.22 / 8185.08|1.055 / 1.012|13270.07 / 13116.03|0.988 / 0.988|Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|hits|1|q35|1751.10 / 2058.06|1.163 / 1.175|1153.09 / 751.75|0.652 / 0.647|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|1|q36|49.14 / 52.50|1.058 / 1.068|192.28 / 127.37|0.662 / 0.631|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|1|q37|8.74 / 23.69|2.832 / 2.711|96.47 / 70.52|0.731 / 0.676|Relative time flag with at most20ms absolute delta; retained separately. Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|1|q39|88.29 / 98.48|1.132 / 1.115|301.47 / 222.92|0.739 / 0.725|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|1|q40|3.63 / 4.97|1.476 / 1.369|93.08 / 66.34|0.713 / 0.652|Relative time flag with at most20ms absolute delta; retained separately. Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|1|q41|5.11 / 5.75|1.033 / 1.126|88.18 / 66.36|0.753 / 0.696|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|1|q42|9.59 / 9.42|0.996 / 0.982|82.43 / 65.11|0.790 / 0.738|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q00|0.09 / 0.09|0.899 / 0.996|48.81 / 56.92|1.166 / 1.249|Higher whole-process RSS; peak regression remains visible.|
|hits|14|q01|1.96 / 2.86|1.498 / 1.459|171.36 / 70.58|0.412 / 0.350|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|14|q02|14.56 / 20.10|1.370 / 1.380|320.32 / 73.43|0.229 / 0.188|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|14|q04|126.00 / 167.84|1.345 / 1.332|980.47 / 518.57|0.529 / 0.521|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|14|q05|196.72 / 251.64|1.325 / 1.279|2078.46 / 1025.08|0.493 / 0.489|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee. Large absolute peak; capacity planning must use this row.|
|hits|14|q06|0.52 / 0.23|0.443 / 0.449|69.94 / 59.13|0.845 / 0.798|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q07|3.09 / 4.05|1.325 / 1.312|173.56 / 71.96|0.415 / 0.354|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|14|q08|154.66 / 193.80|1.294 / 1.253|1217.35 / 711.13|0.584 / 0.579|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|14|q09|257.26 / 314.75|1.295 / 1.223|1512.55 / 726.32|0.480 / 0.475|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|14|q11|61.44 / 78.83|1.253 / 1.283|890.62 / 283.74|0.319 / 0.306|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|14|q16|364.80 / 451.24|1.276 / 1.237|3805.41 / 2423.57|0.637 / 0.635|Time limitation with modest RSS benefit; explicitly retained, not averaged away. Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|hits|14|q17|294.27 / 379.04|1.371 / 1.288|3751.44 / 2419.08|0.645 / 0.643|Time limitation with modest RSS benefit; explicitly retained, not averaged away. Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|hits|14|q19|2.07 / 5.02|2.628 / 2.424|267.89 / 73.68|0.275 / 0.228|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|14|q27|184.87 / 250.91|1.280 / 1.357|5003.87 / 308.41|0.062 / 0.059|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee.|
|hits|14|q28|3591.36 / 5014.49|1.450 / 1.396|5611.19 / 1961.49|0.350 / 0.348|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee. Large absolute peak; capacity planning must use this row.|
|hits|14|q29|15.17 / 22.06|1.451 / 1.454|231.31 / 70.98|0.307 / 0.254|Relative time flag with at most20ms absolute delta; retained separately.|
|hits|14|q32|600.48 / 760.14|1.204 / 1.266|8710.32 / 7970.26|0.915 / 0.915|Time limitation with modest RSS benefit; explicitly retained, not averaged away. Limited per-query RSS reduction; suite-level saving does not apply to this point. Large absolute peak; capacity planning must use this row.|
|hits|14|q33|674.41 / 854.45|1.435 / 1.267|11614.77 / 5989.92|0.516 / 0.515|Substantive time/RSS tradeoff; absolute slowdown retained; no per-query speed guarantee. Large absolute peak; capacity planning must use this row.|
|hits|14|q35|218.84 / 240.54|1.115 / 1.099|1425.24 / 873.07|0.613 / 0.608|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q36|11.12 / 11.82|1.059 / 1.062|250.56 / 188.32|0.752 / 0.734|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q37|2.87 / 5.22|1.885 / 1.816|133.23 / 101.40|0.761 / 0.728|Relative time flag with at most20ms absolute delta; retained separately. Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q38|4.08 / 4.85|1.362 / 1.187|150.77 / 99.60|0.661 / 0.619|Relative time flag with at most20ms absolute delta; retained separately. Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q39|21.02 / 24.93|1.151 / 1.186|401.11 / 289.01|0.721 / 0.709|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q40|1.95 / 2.35|1.217 / 1.208|116.62 / 95.31|0.817 / 0.788|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q41|2.03 / 2.15|1.152 / 1.060|107.33 / 94.96|0.885 / 0.864|Limited per-query RSS reduction; suite-level saving does not apply to this point.|
|hits|14|q42|2.86 / 2.88|1.005 / 1.008|100.32 / 88.10|0.878 / 0.855|Limited per-query RSS reduction; suite-level saving does not apply to this point.|

## COPY calibration kept separate

The same2-million-row input, four original profiles, three samples/engine and opposite-engine full-payload/order checks were retained. The retired unaccounted87ms/665MB Rust path was not rerun. The owner’s explicitly approved14-thread/256MB memory-bound condition is satisfied by the accepted macro baseline; its current profile is15.6% slower with approximately equal median RSS. The other profiles remain individually visible limitations, not a claim of per-profile parity.

| Threads / limit / skew | C++ / Rust median ms | C++ / Rust median RSS MB | C++ / Rust max RSS MB | Successful time ratio | C++ / Rust errors |
|---|---:|---:|---:|---:|---:|
|1 / 256MB / False|282.96 / 389.06|114.85 / 167.66|114.92 / 167.71|1.375|0 / 0|
|14 / 256MB / False|238.44 / 275.55|456.75 / 452.15|463.55 / 471.11|1.156|0 / 0|
|14 / 512MB / False|164.52 / 220.12|709.97 / 577.99|726.88 / 584.70|1.338|0 / 0|
|14 / 256MB / True|176.85 / 326.52|390.56 / 639.50|410.78 / 653.15|N/A: no successful C++ counterpart|3 / 0|

Single-thread COPY is37.5% slower and uses46.0% more median whole RSS (about53MB extra): a dominated profile, explicitly retained as a limitation. At14 threads/512MB Rust is33.8% slower with18.6% less median RSS. The skewed C++ attempts fail three times while Rust completes; their early-error duration is not a successful timing comparison, and no speedup is credited.

## Five original whole-file stress outliers

These use literal original SQL/data, default engine thread settings,16GiB engine limit,20GiB RSS guard,900s watchdog and three native repetitions each. All30 whole-file runs passed. They are distinct from TPC-H/ClickBench query timings and the four COPY profiles. The following remaining performance limitations are not represented as matching C++ or meeting the macro tradeoff per file.

| Original file | C++ / Rust median s | Rust/C++ time | C++ / Rust max RSS GB | RSS ratio | Review |
|---|---:|---:|---:|---:|---|
|test/sql/storage/parallel/reclaim_space_batch_insert.test_slow|20.994 / 65.405|3.115|0.284 / 0.220|0.774|Large slowdown with lower peak RSS; retained as a non-macro stress limitation.|
|test/parquet/variant/variant_roundtrip.test_slow|7.385 / 39.808|5.391|9.195 / 3.456|0.376|Large slowdown with lower peak RSS; retained as a non-macro stress limitation.|
|test/sql/copy/csv/test_multiple_big_compressed_csvs.test_slow|64.454 / 96.325|1.494|4.019 / 4.429|1.102|Slower with little/no RSS benefit; retained as a non-macro performance limitation.|
|test/sql/parallelism/interquery/concurrent_checkpoint_wal_optimistic_index.test_slow|10.413 / 31.680|3.042|1.360 / 0.806|0.593|Large slowdown with lower peak RSS; retained as a non-macro stress limitation.|
|test/sql/storage/parallel/reclaim_space_primary_key_optimistic.test_slow|13.089 / 32.843|2.509|0.214 / 0.210|0.977|Slower with little/no RSS benefit; retained as a non-macro performance limitation.|

The compressed-CSV file is1.494x time/1.102x peak RSS; primary-key optimistic reclaim is2.509x time/0.977x RSS. VARIANT round-trip is5.391x time/0.376x RSS. Batch reclaim is3.115x/0.774x; checkpoint/WAL/index is3.042x/0.593x. None is hidden in a macro average or used to justify additional caching. Their literal cases, results, timing/RSS and raw logs remain preserved.

## Evidence and scope

The unchanged historical aggregate policy is documented in `docs/performance-final-native-20261029.md` and its signed review. Current raw summaries: `performance-final-source-2742/summary.json`, independent numeric review2743, tied-answer controls2744, COPY2723 and whole-file2724. This review adds scoped acceptance and individual dispositions; it does not mutate those immutable reports’ original `performance_accepted: false` fields.

Rust native SHA: `7399ed8ea8c2ebff8b86f4d9de3e47c71ad97e8d7699e1b8439c5c8c7afd819f`. C++ reference SHA: `8db9a7ccb5971b8316820f3bb387f08a2b74c47d2bbfff1cc62c8ebf6e7c0ca1`. Whole release acceptance still requires actual signed-artifact runtime/path checks and required platform evidence. Physical iOS remains unverified and not waived. No additional language clients, Intel Mac support, exact performance parity or outperformance are inferred.
