# Current coupled whole-file outliers

Final source2551/Mac2556 measurements are complete (November 6, 2026): all five original files passed three native repetitions per engine, with literal inputs, default threads, 16GiB engine budget, 20GiB RSS guard and 900-second watchdog. These are whole-file stress measurements, separate from TPC-H/ClickBench and COPY calibration. No cache or memory budget changed.

| Original file | Rust/C++ median time | Rust/C++ max whole RSS |
|---|---:|---:|
| Batch-insert reclaim | 3.115 | 0.774 |
| VARIANT round-trip | 5.391 | 0.376 |
| Multiple big compressed CSVs | 1.494 | 1.102 |
| Concurrent checkpoint/WAL/index | 3.042 | 0.593 |
| Primary-key optimistic reclaim | 2.509 | 0.977 |

All 30 runs, absolute times/peaks and raw logs are preserved in the [final whole-file proof](../.context/artifacts/platform-matrix-20261029/final-SQL-file-outlier-measurements-2724/verification.json). The [current acceptance report](performance-final-native-20261106.md) lists every limitation, all four COPY profiles and all 63 native query flags. A macro aggregate acceptance does not claim per-file C++ performance parity or hide CSV's higher RSS.

Earlier targeted experiments below retain their original historical scope; their pending/running labels do not describe the completed final collection.

# Current coupled performance outliers

Measured November4 UTC / November5 local,2026. Source2253; signed macOS arm64 artifacts2268. Engine/cache/memory policy is unchanged. Performance acceptance is pending.

The following rows retain the complete original SQL-file workloads, including startup and finalization. Each engine/file has three samples; all30 original-file replays passed. RSS is the maximum Darwin kernel peak across those samples. Individual rows remain separate from final TPC-H/ClickBench acceptance.

| Original SQL file | C++ median s | Rust median s | Rust/C++ time | C++ max RSS GB | Rust max RSS GB | Rust/C++ RSS |
|---|---:|---:|---:|---:|---:|---:|
| `test/parquet/variant/variant_roundtrip.test_slow` |7.312|56.504|7.728|9.383|3.531|0.376|
| `test/sql/storage/parallel/reclaim_space_batch_insert.test_slow` |20.778|65.449|3.150|0.285|0.220|0.769|
| `test/sql/copy/csv/test_multiple_big_compressed_csvs.test_slow` |63.762|94.842|1.487|4.202|4.265|1.015|
| `test/sql/parallelism/interquery/concurrent_checkpoint_wal_optimistic_index.test_slow` |10.978|33.036|3.009|1.337|0.825|0.617|
| `test/sql/storage/concurrent_attach.test_slow` |1.374|25.943|18.882|0.052|0.037|0.711|

The18.88x concurrent-attach and7.73x VARIANT roundtrip slowdowns are severe individual observations. Reclaim-space batch insertion and optimistic checkpoint/index concurrency are about3x slower. Multiple compressed CSVs are about1.49x slower at similar maximum RSS. These results require a documented disposition; they are not accepted by aggregation.

COPY uses the original2-million-row fixture and four profiles, three samples per engine. The following table reports median elapsed time and median whole-process peak RSS. Successful outputs were checked through the opposite engine for complete values and order.

| Threads / budget / data | C++ median ms | Rust median ms | Successful time ratio | C++ median RSS MB | Rust median RSS MB | C++ / Rust errors |
|---|---:|---:|---:|---:|---:|---:|
|1 / 256MB / normal|284.21|385.53|1.356|122.96|164.35|0 / 0|
|14 / 256MB / normal|234.02|246.65|1.054|451.54|457.06|0 / 0|
|14 / 512MB / normal|163.73|206.80|1.263|701.45|649.36|0 / 0|
|14 / 256MB / skewed|168.24|342.42|unavailable|388.40|643.07|3 / 0|

The14-thread/256MB normal profile is about5.4% slower at similar median RSS. The single-thread profile is about35.6% slower with33.7% higher median whole-process RSS. The14-thread/512MB profile is about26.3% slower with7.4% lower median RSS. In the skewed profile all three C++ runs returned OOM while all three Rust runs succeeded; the recorded RSS and outcome difference remain explicit, and no successful speed ratio is inferred from C++ error time.

The owner accepts the lower-memory TPC-H/ClickBench tradeoff and the previously measured memory-bound COPY exception. Current native macro measurements, individual-query review and a disposition for the severe whole-file outliers remain required before performance acceptance.

Evidence: [30 whole-file samples and groups](../.context/artifacts/platform-matrix-20261029/corrected-SQL-file-outlier-measurements-2364/verification.json), [24 COPY samples and profiles](../.context/artifacts/platform-matrix-20261029/corrected-COPY-speed-memory-2363/verification.json). No raw outcomes or acceptance ratchets were rewritten.


## Checkpoint attribution and sync policy

A diagnostic retaining both1000-loop workers and the original SQL statement bodies reproduced1.26s C++ and26.90s Rust through signed public C API libraries. Summed statement times across both workers were: checkpoint1.19s C++/39.48s Rust; aggregate/insert1.03s/11.94s; attach0.215s/0.617s; detach0.060s/0.123s. These diagnostic times remain separate from the original whole-file acceptance samples.

Native sampling and disassembly reveal a configured sync-policy mismatch: the frozen C++reference FileSync callsfsync directly, while the current Rust standard-library Apple sync_data requestsfcntl(F_FULLFSYNC=51). The C++source contains an optionalHAVE_FULLFSYNC branch that is absent from this configured binary. This explains a major checkpoint cost; it does not justify skipping synchronization or silently changing durability. An isolated source2523 experiment is evaluating the matching C++fsync operation with original ordering, registered-backend delegation, data/error controls and no memory/cache change. The source2253 release candidate is unchanged and no experiment has been promoted.

Evidence: [statement attribution](../.context/artifacts/platform-matrix-20261029/attach-statement-diagnostic-2520/rust/verification.json), [native sync-policy evidence](../.context/artifacts/platform-matrix-20261029/apple-sync-semantics-audit-2522/verification.json), [experimental source](../.context/artifacts/platform-matrix-20261029/source-2523/manifest.json).

The isolated source2523 evaluation completed successfully: all64 storage unit tests passed, including actual data persistence and kernel-error propagation;20 bidirectional general checkpoint/WAL journeys,40 tail recoveries and110 native processes also passed. Three full original concurrent-attach samples per engine measured median1.6689s Rust versus1.3668s C++ (1.221x), with maximum whole-process RSS38.63MB versus51.86MB (0.745x). These preserve the full1000-loop/two-worker file and original assertions. The baseline source2253 results above remain unchanged. This is targeted experimental evidence, not promotion or final release acceptance.

Evidence: [signed sync experiment and storage controls](../.context/artifacts/platform-matrix-20261029/apple-fsync-evaluation-2526/verification.json), [six original-file speed and kernel-RSS samples](../.context/artifacts/platform-matrix-20261029/apple-fsync-original-attach-measurements-2528/verification.json).

VARIANT profiling2529 completed the original full roundtrip file successfully and located substantial allocation/copy work in Parquet variant conversion, including rebuilding field names and nested logical types per row. An isolated schema-reuse experiment2533 is under strict/unit/build verification2534 before signed full-file speed/memory comparison2537. Its test-only original algorithm compares values, complete types, NULLs and errors across scalar, object, array and mixed shredding cases. No cache or memory policy is added; no production candidate is promoted. The first2530 strict check failed only a test semicolon lint; its raw failure2531 is retained, and2533 differs by that semicolon correction.

The schema-reuse experiment2533 completed strict checks, its original-algorithm comparison, the Parquet unit suite (30 passes, two diagnostic ignores), and six unchanged whole-file roundtrip samples. Median time was45.3496s Rust /7.2160s C++ (6.2846x); maximum kernel RSS was3.48365GB /7.54128GB (0.46194x). Rust time is about19.7% lower than baseline2364, with slightly lower peak RSS. C++ RSS varied between cohorts, so the per-cohort measurements remain separate. The severe individual slowdown is still open. Focused66-file original VARIANT comparison2539 and new native profile2540 follow serially; no production promotion or final performance acceptance.

Evidence: [schema reuse controls and signed build](../.context/artifacts/platform-matrix-20261029/variant-schema-evaluation-2538/verification.json), [six unchanged roundtrip samples](../.context/artifacts/platform-matrix-20261029/variant-schema-original-file-measurements-2537/verification.json).

The combined direct-vector experiment2542 passed strict checks, both meaningful comparisons, the Parquet unit suite (31 passes, two diagnostic ignores) and all66 original VARIANT SQL files (65PASS/1same require-inetSKIP, zero differences). Three full original VARIANT roundtrip samples per engine measured median38.6423s Rust /7.2674s C++ (5.3172x), maximum kernel RSS3.49631GB /9.03791GB (0.38685x). The Rust median is31.6% lower than source2253's original56.5041s, with essentially the same lower-memory footprint. The residual slowdown remains explicit; these are whole-file COPY/read journeys, not single-query macro measurements.

All12 additional complete original outlier replays2549 passed. Reclaim-space median time remains3.0815x C++ with0.77150x maximum peak RSS. Concurrent checkpoint/WAL/index remains3.3925x with0.61425x RSS. The sync correction does not remove these separate costs. The original compressed-CSV observation remains in its original source2253 cohort; it has not been silently replaced or attributed to the new source.

The targeted-verified sync/schema/direct-vector fixes were applied to four guarded working-tree source files2550. The duplicated old-algorithm experimental test stays in immutable experimental evidence; the working tree uses compact fixed-wire/NULL/alias and vector-selection controls. Coherent source2551 includes these changes and the previously verified GNU shell diagnostic correction. Final build/sign2555/2556 and full4870 Mac collection/review2560/2561 are queued serially. These new final artifacts still require their own full client, shell, persistence/WAL, platform and coupled-macro verification. No final performance or release acceptance is granted by the experiments.

Evidence: [combined experiment controls and samples](../.context/artifacts/platform-matrix-20261029/variant-vector-evaluation-2547/verification.json), [original66-file comparison](../.context/artifacts/platform-matrix-20261029/variant-vector-focused-corpus-2548/verification.json), [12 other outlier samples](../.context/artifacts/platform-matrix-20261029/sync-and-variant-remaining-outlier-measurements-2549/verification.json), [guarded working-tree patch](../.context/artifacts/platform-matrix-20261029/working-tree-sync-variant-fixes-2550/verification.json).

Count correction2567: the final-source twelve signed component harnesses have132PASS/0FAIL/2diagnosticIGNORED. The two Snappy comparison/benchmark diagnostics were included in earlier total-case labels but were not executed or accepted. The final Parquet library unit result is31PASS/0FAIL/2IGNORED. All raw logs remain unchanged. [Exact signed component test review](../.context/artifacts/platform-matrix-20261029/final-source-signed-component-tests-review-2567/verification.json).
