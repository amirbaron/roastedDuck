# Performance backlog

Known places where rustdb is slower or heavier than DuckDB 1.5.6. Every item must be
closed before M9 (no benchmark below 0.99× C++ speed, memory geomean ≤ 0.6×). Numbers are
CPU time unless noted; the machine is shared, so small differences need re-checking.

## Benchmark suites (round 2)

`scripts/perf/bench.py` runs TPC-H, h2oai (groupby and join) and a ClickBench subset on
both engines; `scripts/perf/data.sh` creates the databases with DuckDB's CLI (both engines
read the same file) and `scripts/perf/suites.sh` / `scripts/perf/table.py` run and tabulate
whole suites. Each (engine, query) runs in its own process that drives the engine through
the C API (DuckDB's release `libduckdb`, rustdb's `crates/capi`): one warm-up run whose
result is checked, then 5 timed runs (best wall time; CPU time of that run); peak RSS is the
process's. Results are compared value by value (doubles to 1e-9); `ORDER BY ... LIMIT`
queries whose ties make the order arbitrary are checked by hashing the whole result
without ORDER BY / LIMIT on both engines.

Data (disk-limited machine): TPC-H **SF3** instead of SF10 (2.6 GB would not fit next to
the other worktrees), h2oai `G1_1e7_1e2_5_0` and `J1_1e7_*` (DuckDB's benchmark data,
10M rows), ClickBench **3 of the 100 `hits` partitions** (3M rows). Times are in-process
and warm, unlike the per-process numbers of round 1 below, which included start-up and
opening the database.

| run | suite | threads | DuckDB wall s | rustdb wall s | speed geomean | slowest | mem geomean | max mem |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| baseline | tpch3 | 14 | 0.484 | 0.667 | 0.68× | q22 0.33× | 0.95× | q22 1.18× |
| baseline | h2og | 14 | 0.502 | 3.335 | 0.35× | q08 0.05× | 0.84× | q08 1.91× |
| baseline | h2oj | 14 | 0.697 | 5.776 | 0.12× | q04 0.10× | 0.91× | q05 0.98× |
| baseline | hits | 14 | 0.512 | 0.916 | 0.61× | q29 0.16× | 0.97× | q26 1.55× |
| baseline | tpch3 | 1 | 3.218 | 4.083 | 0.72× | q02 0.47× | 0.99× | q21 1.08× |
| baseline | h2og | 1 | 3.319 | 5.021 | 0.65× | q08 0.28× | 0.92× | q08 1.03× |
| baseline | h2oj | 1 | 7.355 | 6.426 | 1.17× | q05 1.01× | 0.58× | q05 0.64× |
| baseline | hits | 1 | 3.450 | 5.346 | 0.52× | q29 0.05× | 1.03× | q23 1.68× |

## TPC-H SF1 status

Measured with `.context/perf/bin/tpch.py` (one process per query, best of 5, warm OS
cache; the database file is DuckDB's `CALL dbgen(sf=1)` output). Wall / CPU seconds summed
over the 22 queries; peak RSS is the largest of any query; the ratios are geomeans over the
queries of DuckDB time / rustdb time and rustdb RSS / DuckDB RSS.

| | threads | rustdb wall | rustdb CPU | DuckDB wall | DuckDB CPU | rustdb RSS | DuckDB RSS | speed | memory |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| before parallel execution | 1 | 3.46 | 3.00 | 1.65 | 1.31 | 236 MB | 252 MB | | |
| before parallel execution | 14 | 3.45 | 3.15 | 0.81 | 2.52 | 236 MB | 292 MB | | |
| parallel executor, sort port, memory limit | 1 | 1.88 | 1.59 | 1.42 | 1.10 | 237 MB | 252 MB | | |
| parallel executor, sort port, memory limit | 14 | 0.94 | 4.88 | 0.67 | 2.56 | 262 MB | 294 MB | | |
| now | 1 | 1.66 | 1.36 | 1.50 | 1.16 | 240 MB | 252 MB | 0.91× | 0.94× |
| now | 14 | 0.62 | 2.91 | 0.62 | 2.58 | 268 MB | 295 MB | 1.03× | 0.92× |

Results are identical to DuckDB's for all 22 queries at 1 and 14 threads. Slowest
queries now: Q21 (0.72× / 0.81×), Q16 (0.75× / 0.92×), Q17 (0.78× / 0.98×), Q3 and Q10
(0.8× single-threaded).

Memory-heavy queries (`.context/perf/bin/mem.py`, SF1 lineitem): all run under a 100 MB
limit at 4 threads with peak RSS within 0.9–1.2× of DuckDB; without a limit, sorting
`l_comment` takes 651 MB vs 457 MB and 1.15 s vs 0.81 s.

## Open items

| Area | Gap | Suspected cause | Found in |
|---|---|---|---|
| Peak memory | ~0.92× DuckDB on TPC-H, target ≤ 0.6× | the scanned table blocks stay cached in the buffer pool like DuckDB's; hash tables and rows are as wide as or wider than DuckDB's (Rust `i128` states align to 16) | `crates/buffer`, `crates/row` |
| Right delim joins (Q17, Q20, Q21) | input buffered and rescanned into the join's build | DuckDB's `PhysicalRightDelimJoin` sinks straight into the join's hash table and the distinct aggregate; here a delim sink copies the input and a cached scan feeds the build pipeline | `crates/execution` (`operators/delim.rs`, `plan_generator.rs`) |
| Hash join probe | `get_row_pointers` + prefetching ~1.4× DuckDB's `GetRowPointers` | per-row bookkeeping in the probe loop; per-call `ChunkFormat` allocation | `crates/execution` (`join_hashtable.rs`) |
| Aggregate hash table lookups | ~1.5× DuckDB's `FindOrCreateGroupsInternal` per row; row matching ~2× | rows are wider (16-byte state alignment); match loop goes through closures | `crates/execution` (`aggregate_hashtable.rs`), `crates/row` (`matcher.rs`) |
| Sorting VARCHAR keys | `l_comment` sort 1.4× slower, 1.4× the memory | the output decodes strings from the sort keys (`decode_value`) instead of reading a payload; contiguous copy of each run's keys | `crates/row` (`sort/`) |
| Bit-packed scans | unpacking into a temporary and copying | DuckDB decodes whole vectors and can select rows while decoding (`BitpackingSelect`) | `crates/storage` (`compression/bitpacking.rs`) |
| Dictionary-compressed strings | scans copy one `string_t` per row | DuckDB emits dictionary vectors (`ScanToDictionaryVector`) that later operators handle once per distinct value | `crates/storage` (`compression/dictionary.rs`) |
| Window source | one hash group per thread | DuckDB splits a hash group into tasks (`WindowHashGroup` stages), so a single partition runs on one thread here | `crates/execution` (`operators/window.rs`) |
| IEJoin / AsOf sinks | sequential | their sinks switch inputs on `combine` instead of DuckDB's two finalize events (`AddFinishEvent`) | `crates/execution` (`operators/join/iejoin.rs`, `asof_join.rs`) |
| Streaming LIMIT | pipelines with a LIMIT run on one thread | DuckDB's `PhysicalStreamingLimit` is parallel when order does not matter, and `PhysicalLimit` collects batches in parallel | `crates/execution` (`operators/limit.rs`) |
| Common subexpressions | `l_extendedprice * (1 - l_discount)` computed twice in Q1 | check the CSE optimizer port against DuckDB's plan | `crates/optimizer` |
| Parser: tiny statements (`SELECT 1`, short SELECTs) | ~1.05–1.25× DuckDB cycles per query | per-node malloc/free vs DuckDB's arena-allocated parse tree | `crates/parser` (`parse_bench`) |
| Spill files | compression has one level | `ruzstd` only encodes at about zstd level 1; DuckDB adapts between levels -5..5 | `crates/buffer` (`temp_file.rs`) |
| Nested comparisons (LIST/STRUCT/ARRAY `=`, `<`, …) | not measured | row by row through `Value`s; port DuckDB's vectorised `PositionComparator` (`is_distinct_from.cpp`) | `crates/types` (`compare.rs`) |
| Cross product / UNNEST constant side | not measured | one `Value` per output chunk instead of a zero-copy constant view of a row (`ConstantVector::Reference`) | `crates/execution` |
| Reservoir sample storage | not measured | sampled rows are copied value by value into the reservoir (`crates/execution/src/sample.rs::append_rows`); needs a typed `VectorOperations::Copy` with selection | `crates/execution` |

## Closed

| Area | Was | Fix |
|---|---|---|
| Single-threaded execution | every pipeline on one thread | DuckDB's task scheduler, meta pipelines and event graph (`crates/execution/src/parallel`); parallel scans, joins, aggregates, sort, window |
| Hash join finalize | single-threaded pointer table build | parallel init + insert stages (`HashJoinTableInitEvent` / `HashJoinFinalizeEvent`) |
| Hash aggregate finalize | all partitions merged and materialised in `finalize` | partitions merged and emitted by parallel source tasks; a single thread's table is emitted without merging (`single_ht`) |
| Hash aggregate sink | every thread grew one large table | DuckDB's strategy: grow up to 2 threads, else small cache-resident tables that start over, HLL-based adaptation (skip lookups / grow), radix bits raised as partitions fill; groups without validity when statistics prove no NULLs |
| `DISTINCT` aggregates | one global mutex-protected table, never spilled | thread-local partitioned tables, parallel per-partition finalize, set aside and spilled under memory pressure |
| `memory_limit` | stored but not applied (100 MB limit used ~885 MB) | applied to the buffer manager; default temp directory; out-of-core hash join and aggregate; zstd-compressed spill files |
| Sort: VARCHAR / multi-key keys, tie order | custom radix sort, ties in a different order than DuckDB | ported vergesort + ska_sort + pdqsort_branchless; ties match DuckDB |
| Vector hashing | per-row type dispatch | loops monomorphised per type |
| FSST / bit unpacking / dictionary scans / constant filters | per-symbol copies, generic unpack, per-row string decode, branchy selection | 8-byte symbol writes, width-specialised unpack kernels, per-segment string cache, branchless typed selection |
| Filters | comparisons materialised a BOOLEAN vector, then branchy partition | comparisons select straight into selection vectors, branchless, one loop per operator |
| Scalar kernels | per-row validity/selection lookups; operations through function pointers; overflow checks always on | NULL-free fast paths, operations inlined, infallible kernels without error paths, overflow-free kernels when statistics allow (`PropagateNumericStats`) |
| Scan result vectors | ~5% of CPU allocating and zeroing vectors | per-thread recycling of vector buffers; scans reuse them without clearing |
| Buffered chunks (delim join, CTE, cross product, results) | kept whole scanned vectors alive (Q17: 201 MB) | copied into a `ColumnDataCollection` or compacted (Q17: 80 MB) |
| DECIMAL types across threads | reference-count contention on the shared type parameters (7% of Q10 at 14 threads) | interned DECIMAL parameters |
| LIKE / contains | substring searcher built per row | built once per constant pattern |
