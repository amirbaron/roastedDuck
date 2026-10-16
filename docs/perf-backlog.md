# Performance backlog

Known places where rustdb is slower or heavier than DuckDB 1.5.6. Every item must be
closed before M9 (no benchmark below 0.99× C++ speed, memory geomean ≤ 0.6×). Numbers are
CPU time unless noted; most were measured on a heavily loaded machine and need re-checking.

## TPC-H SF1 status

Measured with `.context/perf/bin/tpch.py` (one process per query, best of 3, warm OS
cache; the database file is DuckDB's `CALL dbgen(sf=1)` output). Wall / CPU seconds summed
over the 22 queries; peak RSS is the largest of any query.

| | threads | rustdb wall | rustdb CPU | DuckDB wall | DuckDB CPU | rustdb RSS | DuckDB RSS |
|---|---:|---:|---:|---:|---:|---:|---:|
| before parallel execution | 1 | 3.46 | 3.00 | 1.65 | 1.31 | 236 MB | 252 MB |
| before parallel execution | 14 | 3.45 | 3.15 | 0.81 | 2.52 | 236 MB | 292 MB |
| now | 1 | 1.88 | 1.59 | 1.42 | 1.10 | 237 MB | 252 MB |
| now | 14 | 0.94 | 4.88 | 0.67 | 2.56 | 262 MB | 294 MB |

Results are identical to DuckDB's for all 22 queries at 1 and 14 threads.

## Open items

| Area | Gap | Suspected cause | Found in |
|---|---|---|---|
| CPU efficiency overall | ~1.4–2× DuckDB CPU per TPC-H query, at any thread count | spread over the items below; single-threaded profile: scans 37%, hash join probe 13%, hash aggregate sink 11%, filters 8% | `crates/*` |
| Bit-packed scans | ~13% of single-threaded TPC-H CPU | decoding 32 values per call through the segment scan; DuckDB decodes whole vectors and can select rows while decoding (`BitpackingSelect`) | `crates/storage` (`compression/bitpacking.rs`) |
| Scan result vectors | ~5% of CPU in `Vector::flat` (allocate + zero) | a scanned vector's buffer is still referenced downstream (slices, clones) when the next chunk is scanned, so it is reallocated; DuckDB re-points vectors at their cache buffer | `crates/types`, `crates/storage` (`column_data.rs`) |
| Dictionary-compressed strings | scans copy one `string_t` per row | DuckDB emits dictionary vectors (`ScanToDictionaryVector`) that later operators handle once per distinct value | `crates/storage` (`compression/dictionary.rs`) |
| Hash join probe | ~13% of single-threaded CPU | pointer chasing in `JoinHashTable::probe`/`ScanStructure::next`; per-call `ChunkFormat` allocation | `crates/execution` (`join_hashtable.rs`) |
| Hash aggregate | `find_or_create_groups` + `resize` ~8% | no HLL-based adaptivity (`DecideAdaptation`), no abandon/grow strategy split by thread count | `crates/execution` (`aggregate_hashtable.rs`, `operators/hash_aggregate.rs`) |
| Out-of-core aggregate | 2–6× DuckDB wall under a tight `memory_limit` | abandoned groups are repartitioned on every abandon and read back from temp files; threshold is a fixed share of the limit, not a `TemporaryMemoryManager` reservation | `crates/execution` (`operators/hash_aggregate.rs`) |
| Sort peak memory | 644 MB vs DuckDB 458 MB (ORDER BY of `lineitem.l_comment`, 14 threads) | contiguous copy of each run's keys for sorting (skipped under memory pressure) | `crates/row` (`sort/run.rs`) |
| Grouped aggregate peak memory | 618 MB vs 542 MB (GROUP BY `l_comment`) | wider rows than DuckDB's layout | `crates/execution` |
| Window source | one hash group per thread | DuckDB splits a hash group into tasks (`WindowHashGroup` stages), so a single partition runs on one thread here | `crates/execution` (`operators/window.rs`) |
| IEJoin / AsOf sinks | sequential | their sinks switch inputs on `combine` instead of DuckDB's two finalize events (`AddFinishEvent`) | `crates/execution` (`operators/join/iejoin.rs`, `asof_join.rs`) |
| Streaming LIMIT | pipelines with a LIMIT run on one thread | DuckDB's `PhysicalStreamingLimit` is parallel when order does not matter | `crates/execution` (`operators/limit.rs`) |
| Result collection | one cloned chunk per output chunk | DuckDB's `ColumnDataCollection` compacts small chunks | `crates/execution` (`operators/result_collector.rs`) |
| Parser: tiny statements (`SELECT 1`, short SELECTs) | ~1.05–1.25× DuckDB cycles per query | per-node malloc/free vs DuckDB's arena-allocated parse tree | `crates/parser` (`parse_bench`) |
| Spill files | uncompressed, no block header | zstd encoder not wired | `crates/buffer` |
| Nested comparisons (LIST/STRUCT/ARRAY `=`, `<`, …) | not measured | row by row through `Value`s; port DuckDB's vectorised `PositionComparator` (`is_distinct_from.cpp`) | `crates/types` (`compare.rs`) |
| Cross product / UNNEST constant side | not measured | one `Value` per output chunk instead of a zero-copy constant view of a row (`ConstantVector::Reference`) | `crates/execution` |
| Reservoir sample storage | not measured | sampled rows are copied value by value into the reservoir (`crates/execution/src/sample.rs::append_rows`); needs a typed `VectorOperations::Copy` with selection | `crates/execution` |

## Closed

| Area | Was | Fix |
|---|---|---|
| Single-threaded execution | every pipeline on one thread | DuckDB's task scheduler, meta pipelines and event graph (`crates/execution/src/parallel`); parallel scans, joins, aggregates, sort, window |
| Hash join finalize | single-threaded pointer table build | parallel init + insert stages (`HashJoinTableInitEvent` / `HashJoinFinalizeEvent`) |
| Hash aggregate finalize | all partitions merged and materialised in `finalize` | partitions merged and emitted by parallel source tasks |
| `COUNT(DISTINCT)` without GROUP BY | one global mutex-protected table | thread-local tables, parallel per-partition finalize |
| `memory_limit` | stored but not applied (100 MB limit used ~885 MB) | applied to the buffer manager; default temp directory; out-of-core hash join and aggregate |
| Sort: VARCHAR / multi-key keys, tie order | custom radix sort, ties in a different order than DuckDB | ported vergesort + ska_sort + pdqsort_branchless; ties match DuckDB |
| Vector hashing | per-row type dispatch | loops monomorphised per type |
| FSST / bit unpacking / dictionary scans / constant filters | per-symbol copies, generic unpack, per-row string decode, branchy selection | 8-byte symbol writes, width-specialised unpack kernels, per-segment string cache, branchless typed selection |
