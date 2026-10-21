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

Baseline memory ratios include the Python interpreter's ~16 MB on both sides; later runs
subtract it (peak RSS above the interpreter), which lowers small ratios.

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
| r15 | tpch3 | 14 | 0.558 | 0.677 | 0.80× | q22 0.46× | 0.45× | q11 0.97× |
| r15 | h2og | 14 | 0.582 | 0.690 | 0.88× | q08 0.63× | 0.68× | q06 0.80× |
| r15 | h2oj | 14 | 0.852 | 0.843 | 1.04× | q05 0.87× | 0.76× | q05 0.85× |
| r15 | hits | 14 | 0.587 | 0.715 | 0.87× | q23 0.56× | 0.72× | q41 1.08× |
| r15 | tpch3 | 1 | 3.500 | 3.728 | 0.89× | q11 0.76× | 0.33× | q16 0.90× |
| r15 | h2og | 1 | 3.542 | 4.146 | 0.88× | q09 0.75× | 0.32× | q10 0.70× |
| r15 | h2oj | 1 | 8.259 | 7.358 | 1.15× | q05 0.96× | 0.65× | q05 0.80× |
| r15 | hits | 1 | 3.737 | 4.725 | 0.81× | q19 0.38× | 0.62× | q00 0.95× |

What moved the numbers in round 2 (commits on the round's branch):

- Inserts: DuckDB's parallel `PhysicalInsert` and `PhysicalBatchInsert` (CTAS of the
  h2oai answers was single-threaded: h2og 0.35× → 0.8×, h2oj 0.12× → 1.0×).
- Memory: persistent table blocks are freed when unpinned instead of staying in the
  buffer pool (TPC-H 0.95× → 0.45×); row groups read from the file build their columns on
  first use (DuckDB's lazy column loading); top-N payloads copy their strings; small top-N
  heaps allocate exactly.
- Scans: dictionary vectors from dictionary segments, codec filters (RLE runs, FSST empty
  strings, dictionary filters evaluated once per entry), bit-packed groups decoded
  straight into the result with the frame of reference applied, filter-only columns not
  decoded, reused selection buffers.
- Aggregates: chunks with at most 8 groups update one state at a time in registers
  (`AggregateOps::update_selection`; TPC-H q1 0.78× → 1.2×); all-DISTINCT aggregates skip
  the regular sink (`CanSkipRegularSink`) and a single thread's distinct tuples are not
  deduplicated again; per-dictionary-entry group lookup; typed top-N `arg_min/arg_max`.
- Joins: typed perfect-hash-join probe (branching like DuckDB's, dictionary keys too);
  short string copies without `memcpy` calls.
- Kernels: NULL-aware unary/binary kernels and group locations per validity word; integral
  compressed materialisation through the unary kernel.
- Scheduler: one worker woken per queued task, completion waits on their own condition
  variable (tiny queries 0.6× → 1.1–1.5×); `has_updates` without a lock.

## Benchmark suites (round 3)

Same harness as round 2, on bigger data: TPC-H **SF10** (DuckDB's `CALL dbgen(sf=10)`),
the **full ClickBench `hits`** (100M rows, all 43 queries) and the same h2oai data; 3 timed
runs per query, peak RSS above the interpreter. The machine was shared with other agents
(load 4–20 during the runs), so differences under ~5% are noise; the engines alternate
query by query, so both see the same load.

| run | suite | threads | DuckDB wall s | rustdb wall s | speed geomean | slowest | mem geomean | max mem |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| baseline | tpch10 | 14 | 1.592 | 1.962 | 0.76× | q11 0.34× | 0.32× | q11 0.82× |
| baseline | h2og | 14 | 0.611 | 0.734 | 0.85× | q08 0.56× | 0.68× | q06 0.87× |
| baseline | h2oj | 14 | 0.965 | 0.938 | 1.10× | q05 0.85× | 0.76× | q05 0.82× |
| baseline | hits | 14 | 11.704 | 16.346 | 0.76× | q04 0.37× | 0.84× | q00 8.22× |
| baseline | tpch10 | 1 | 12.039 | 13.447 | 0.85× | q03 0.71× | 0.24× | q16 0.91× |
| baseline | h2og | 1 | 3.355 | 3.959 | 0.86× | q09 0.75× | 0.32× | q06 0.65× |
| baseline | h2oj | 1 | 7.544 | 6.331 | 1.22× | q05 1.04× | 0.61× | q05 0.73× |
| r3 | tpch10 | 14 | 1.462 | 1.729 | 0.83× | q07 0.68× | 0.27× | q11 0.71× |
| r3 | h2og | 14 | 0.579 | 0.671 | 0.89× | q10 0.76× | 0.68× | q10 0.86× |
| r3 | h2oj | 14 | 0.802 | 0.731 | 1.15× | q05 0.90× | 0.76× | q04 0.79× |
| r3 | hits | 14 | 10.406 | 14.505 | 0.80× | q19 0.39× | 0.34× | q00 0.98× |
| r3 | tpch10 | 1 | 11.781 | 12.711 | 0.89× | q16 0.73× | 0.19× | q16 0.77× |
| r3 | h2og | 1 | 3.389 | 4.001 | 0.87× | q05 0.76× | 0.31× | q10 0.75× |
| r3 | h2oj | 1 | 8.103 | 6.778 | 1.22× | q05 1.01× | 0.74× | q03 0.78× |
| r3 | hits | 1 | 99.477 | 130.710 | 0.78× | q19 0.31× | 0.27× | q00 0.98× |

The ClickBench baseline was not measured at one thread. The speed target (≥ 1.3× on every
suite, no query below 0.99×) is not met: only h2oj is faster than DuckDB. The memory target
(geomean ≤ 0.6×, no query above 1.0×) is met everywhere except h2oai's geomeans.

What moved the numbers in round 3:

- Small parallel queries: cross product, expression scan, pivot, table in-out and the
  nested loop, piecewise merge and blockwise joins have DuckDB's parallel flags and
  `MaxThreads`; materialized CTEs pack small chunks; the delim join deduplicates its
  values in parallel, one finalize task per partition (TPC-H q11 0.34× → 0.92×, q22
  0.40× → 0.91×, q21 0.70× → 0.79×).
- `COUNT(DISTINCT)`: the DISTINCT aggregate tables use the radix sink strategy of the
  regular one (grow or start over, HLL adaptation, more radix bits as partitions fill)
  instead of one growing table per thread (ClickBench q04 0.37× → 0.64×).
- Memory: row groups read their columns' metadata only when a column is first used, as
  DuckDB does (opening `hits` 200 MB → 41 MB, q00 8.2× → 0.98×); checkpoint metadata
  blocks are freed when unpinned; 128-bit SUM/AVG states are 8-byte aligned; top-N
  aggregate heaps of up to two entries live in the state. Aggregate pointer tables and
  buffer-allocator data of 1 MiB and more are page mappings of their own: the process
  allocator had kept every pointer table a resize freed (2 GB for ClickBench q32's 100M
  groups at one thread, 1.03× → 0.87×). Unlike DuckDB's `hash_map`, the pointer table is
  still not charged to the buffer pool: with it, TPC-H q9 exceeds a 100 MB limit.
- Joins: bloom filters hash only the rows still selected and look up with the sectors in
  registers; pointer-table inserts prefetch the entries of rows 32 ahead; probes prefetch
  only tables of more than 1M entries (TPC-H q22); probes and scans whose filters keep
  every row skip their selection bookkeeping.
- Top-N sinks drop rows beyond the global boundary before encoding them (DuckDB's
  `CheckBoundaryValues`; ClickBench q26 0.66× → 0.87×).
- Kernels: integer SUM/AVG over whole vectors without overflow checks per row; RLE
  segments decode only the selected rows; string compression for compressed
  materialization without `memcpy`; `IS NOT DISTINCT FROM` on valid input matches as `=`.
- Tried and dropped: a bounded cache of unpinned table blocks (96–256 MB, only blocks read
  a second time). It took ClickBench from 0.80× to 0.84× (q19 0.36× → 0.58×), but memory
  from 0.34× to 0.59×, and small queries went above DuckDB's RSS (h2og q01 doubled,
  ClickBench q24/q26 above 1.0×), so it was removed (`064b45d`). Branch-free row-matcher
  compaction, a fused bloom hash-and-lookup loop and resizing the perfect hash join
  by index were also measured slower and reverted.

## Benchmark suites (round 4)

Same harness and data as round 3. `baseline` is the round 3 code (`930c36a`) measured
again next to the round 4 code (`r4`), each against DuckDB in its own run, the two runs
back to back per suite; 5 timed runs per query for TPC-H and h2oai at 14 threads, 3
otherwise. The machine was shared with other agents' conformance runs (load 4–25). That
moves wall times by up to ±30% between runs of the same query (TPC-H q17 0.83×–1.31×, q19
0.67×–0.82× in two runs of the baseline; an earlier pair of ClickBench runs measured
0.80× → 0.89× at 14 threads), so changes were judged by the cycles and instructions the
process spends instead (`scripts/perf/cycles.sh`: `/usr/bin/time -l` of a process running
a query N times minus one running it once, the binaries alternating), which the load
barely moves; that comparison follows the wall-time table.

| run | suite | threads | DuckDB wall s | rustdb wall s | speed geomean | slowest | mem geomean | max mem |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| baseline | tpch10 | 14 | 1.408 | 1.675 | 0.83× | q07 0.69× | 0.28× | q11 0.72× |
| baseline | h2og | 14 | 0.510 | 0.577 | 0.90× | q09 0.74× | 0.71× | q06 0.91× |
| baseline | h2oj | 14 | 0.707 | 0.667 | 1.10× | q05 0.88× | 0.79× | q05 0.80× |
| baseline | hits | 14 | 10.013 | 13.425 | 0.81× | q19 0.39× | 0.35× | q00 1.02× |
| baseline | tpch10 | 1 | 11.311 | 12.211 | 0.89× | q16 0.73× | 0.19× | q16 0.78× |
| baseline | h2og | 1 | 4.965 | 5.512 | 0.81× | q05 0.50× | 0.31× | q06 0.69× |
| baseline | h2oj | 1 | 7.167 | 6.056 | 1.22× | q05 1.01× | 0.48× | q02 0.59× |
| baseline | hits | 1 | 89.671 | 121.987 | 0.78× | q19 0.31× | 0.26× | q00 1.02× |
| r4 | tpch10 | 14 | 1.418 | 1.578 | 0.86× | q05 0.72× | 0.28× | q11 0.74× |
| r4 | h2og | 14 | 0.511 | 0.515 | 1.00× | q09 0.82× | 0.73× | q10 0.91× |
| r4 | h2oj | 14 | 0.696 | 0.632 | 1.12× | q05 1.00× | 0.79× | q03 0.81× |
| r4 | hits | 14 | 10.240 | 13.395 | 0.85× | q37 0.32× | 0.35× | q00 1.02× |
| r4 | tpch10 | 1 | 11.177 | 11.405 | 0.93× | q16 0.77× | 0.20× | q16 0.79× |
| r4 | h2og | 1 | 3.172 | 3.292 | 0.98× | q05 0.84× | 0.35× | q06 0.71× |
| r4 | h2oj | 1 | 7.315 | 6.159 | 1.20× | q04 1.06× | 0.51× | q04 0.62× |
| r4 | hits | 1 | 88.502 | 110.527 | 0.85× | q19 0.32× | 0.27× | q00 1.03× |

ClickBench q00 (`COUNT(*)`, which only opens the database) is 1 MB above DuckDB's RSS in
both runs (47 MB vs 46 MB; 0.98× in round 3), so it is noise around the line rather than
a change of this round. Memory grew by a few percent on small queries (h2og 0.31× →
0.35× at one thread): the recently released blocks and reused mappings stay resident.

Cycles per query at 14 threads (geomean of DuckDB cycles / rustdb cycles; ClickBench: the
22 slower queries, without q28):

| suite | baseline | r4 | slowest r4 |
|---|---:|---:|---|
| tpch10 | 0.88× | 0.90× | q07 0.75×, q05 0.76×, q02 0.79× |
| h2og | 0.92× | 0.99× | q10 0.84×, q09 0.85×, q04 0.90× |
| hits | 0.77× | 0.79× | q19 0.38×, q01 0.61×, q37 0.62× |

The speed target (≥ 1.3×, nothing below 0.99×) is still far: TPC-H and ClickBench are at
0.85–0.93×, h2og at par, only h2oj is faster than DuckDB. About half of what is left in the
scan-heavy queries is reading table blocks again (see Open items).

What moved the numbers in round 4:

- Aggregates and hash tables: the row matcher's equality loops capture plain data, row
  and selection pointers instead of the input format by reference, and handle stored
  columns that may be NULL and NULL input values (5% of h2oai's groupby columns) in loops
  of their own instead of the generic predicate, which was a call per row; fixed-width
  scatter without NULLs skips the validity check per row; a row's validity bytes are set
  with one store; the first pointer-table lookup iterates flat hashes; `count()` writes its
  results a vector at a time. ClickBench q4 at one thread −17% instructions, q9 −16%;
  h2oai groupby q9 −9%.
- Scans: a scan orders its filters by estimated cost and swaps neighbours at run time when
  that filters faster, as DuckDB's `AdaptiveFilter` does (a join's bloom filter on a key
  column ran before cheap comparisons on later columns and hashed every row; TPC-H q3 at
  one thread spends half as long in it). Small segments of consecutive row groups share
  blocks, which were read again for every segment in them (a third of a single-threaded
  TPC-H q7's reads): such blocks stay among the 16 most recently released blocks. Skipping
  rows moves the scan to the segment it reaches, so that segment's zone map is checked
  before it is read.
- Top-N: the global boundary is checked on the first sort column's values instead of
  encoded sort keys, and with a single sort column rows tying with the boundary are
  dropped, as DuckDB's `CheckBoundaryValues` does (ClickBench q32, which orders 100M groups
  by a count: −52% instructions, −22% cycles).
- Strings: compressed materialization packs strings into integers and unpacks them as
  whole 16-byte values (`TemplatedReverseMemCpy`, `UncompressInlineString`) instead of byte
  by byte; inlined strings are hashed from their 16 bytes (`Hash(string_t)`).
- Inserts: a flat fixed-width vector without NULLs is appended to an uncompressed segment
  in one copy (`StandardFixedSizeAppend`). With the string decompression, h2oai groupby
  q10 (a 10M-row `CREATE TABLE AS`) −15% instructions.
- Allocations of 1 MiB and more (join pointer tables, aggregate tables) are reused for 5 s
  after they are freed, like DuckDB's allocator (jemalloc's decay), instead of having their
  pages faulted in on every run: TPC-H q9 spent 180 ms of its 2.3 s of CPU zeroing fresh
  pages (DuckDB 18 ms). A request no kept mapping fits unmaps them first.
- LIMIT: DuckDB's plan choice (`CreatePlan(LogicalLimit)`): a streaming limit runs on all
  threads when the order does not matter, and a limit of at most 10,000 rows over a
  filtered scan or a join collects the first rows of each batch in parallel
  (`PhysicalLimit`); `test_batch_limit_filters` passes.
- Tried and dropped: filling RLE runs with `slice::fill` (11% more instructions on
  ClickBench q2, whose runs are short); keeping metadata blocks among the recently
  released blocks (it saves the ~8,000 block reads of opening the TPC-H file, but put
  ClickBench q0 2 MB above DuckDB's RSS); 32 instead of 16 recently released blocks (+5–8
  MB on h2oai's small queries, no measurable gain); reading table data through a mapping
  of the file instead of `pread` (a page fault per 16 KiB costs about as much as the copy,
  faults do not scale over threads, and mapped pages count in RSS).

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
| Peak memory | TPC-H 0.20–0.28×, ClickBench 0.27–0.35× (q00, which only opens the database, at 1.02×: 47 MB vs 46 MB), but h2og 0.73× and h2oj 0.79× at 14 threads (target ≤ 0.6×) | the h2o queries are dominated by aggregate and join rows, as wide as DuckDB's; per-thread partitions allocate whole blocks | `crates/row`, `crates/execution` |
| Table block re-reads | `pread`, and waiting for another thread's read of the same block, is ~15% of the CPU of scan-heavy TPC-H queries at 14 threads and most of the gap of ClickBench's point and small scans (q01, q19, q37 at 1.6–2.6× DuckDB's cycles); the copy runs near the memory bandwidth (TPC-H q7's 3,800 blocks: 12 ms at 14 threads, 43 ms at one) | blocks are freed when unpinned to keep memory low (only blocks shared by several segments stay among the 16 last released, round 4); a bounded cache of re-read blocks (round 3, removed in `064b45d`) took q19 to 0.58× and ClickBench to 0.84×, but memory to 0.59× with small queries above 1.0× | `crates/buffer` |
| Metadata re-reads | opening the TPC-H SF10 file reads its 8 metadata blocks 8,161 times (2 GB from the page cache) | every metadata reader pins its block again and metadata blocks are freed when unpinned; keeping them among the recently released blocks fixes it but puts ClickBench q0 2 MB above DuckDB's RSS | `crates/storage` (`metadata.rs`) |
| Right delim joins (Q17, Q20, Q21) | input buffered and rescanned into the join's build | DuckDB's `PhysicalRightDelimJoin` sinks straight into the join's hash table and the distinct aggregate; here a delim sink copies the input and a cached scan feeds the build pipeline | `crates/execution` (`operators/delim.rs`, `plan_generator.rs`) |
| Hash join probe | TPC-H q3/q5/q7 0.72–0.80× at 14 threads; the probe is close to DuckDB's `GetRowPointers` now, bloom filter lookups ~1.3× DuckDB's per key (memory-bound sector loads) | the bloom filter hashes the keys in a separate pass; more rows reach it than in DuckDB when the adaptive filter order has not settled | `crates/execution` (`join_hashtable.rs`, `operators/join/bloom_filter.rs`) |
| Regular expressions with captures | `regexp_replace` 1.5× DuckDB (ClickBench q28 0.67×) | the `regex` crate's bounded backtracker finds the groups; RE2's BitState is faster for anchored patterns spanning the whole string | `crates/functions` (`regexp`) |
| Aggregate hash table lookups | `find_or_create_groups` 1.2–1.5× DuckDB's `FindOrCreateGroupsInternal` (ClickBench q04/q08/q09/q35 0.6–0.8×); resizing at one thread costs ~2× DuckDB's `ReinsertTuples` (q04: 400 ms vs 211 ms) | per-chunk set-up (formats, chunk copies, HLL) and the probe loops cost more instructions; resized pointer tables are fresh mappings whose pages fault in; DuckDB appends a few rows per partition to an unpartitioned collection first (`UNPARTITIONED_RADIX_BITS_THRESHOLD`) | `crates/execution` (`aggregate_hashtable.rs`), `crates/row` |
| Sorting VARCHAR keys | `l_comment` sort 1.4× slower, 1.4× the memory | the output decodes strings from the sort keys (`decode_value`) instead of reading a payload; contiguous copy of each run's keys | `crates/row` (`sort/`) |
| Window source | one hash group per thread | DuckDB splits a hash group into tasks (`WindowHashGroup` stages), so a single partition runs on one thread here | `crates/execution` (`operators/window.rs`) |
| IEJoin / AsOf sinks | sequential | their sinks switch inputs on `combine` instead of DuckDB's two finalize events (`AddFinishEvent`) | `crates/execution` (`operators/join/iejoin.rs`, `asof_join.rs`) |
| Top-N row group order | `top_n_pruning`, `test_top_n_nulls_row_group_pruner_extensive` fail; ORDER BY ... LIMIT scans row groups in storage order | DuckDB's `RowGroupPruner` / `RowGroupReorderer` scan row groups by their statistics and stop once the limit is guaranteed, so the top-N boundary tightens early | `crates/optimizer`, `crates/storage` |
| Struct projection pushdown | `plan_struct_projection_pushdown`, `struct_projection_pushdown_in_storage`, `deliver_child_columns` fail | `struct_extract` on a scanned column is not pushed into the scan as a child column (`ColumnIndex` children, pushdown extract) | `crates/optimizer`, `crates/storage`, `crates/parquet` |
| Partitioned aggregate | `partitioned_group_by` fails | DuckDB's `PhysicalPartitionedAggregate` for groups on hive partition columns is not ported | `crates/execution` |
| Unnest rewriter plans | `unnest_rewriter.test_slow` fails | a lateral `UNNEST` (`CROSS JOIN unnest(arr)`) keeps the `DELIM_JOIN` that DuckDB's `UnnestRewriter` removes | `crates/optimizer` |
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
| Bit-packed scans | unpacking into a temporary and copying | whole groups decode straight into the result, frame of reference (and delta sum) applied in the same pass |
| Dictionary-compressed strings | one `string_t` copy and comparison per row | dictionary vectors (`ScanToDictionaryVector`); pushed-down filters evaluated once per dictionary entry; per-entry group lookup in hash aggregates |
| Aggregates over few groups | every row updated its group's state in memory (store-to-load chains) | chunks with at most 8 groups add each group's rows to its state in one call |
| Tiny queries | every queued task woke every worker | one worker per task (DuckDB's semaphore) |
| Small parallel queries | TPC-H q11/q22 0.34×/0.40× at 14 threads | DuckDB's parallel flags and `MaxThreads` for cross product, expression scan, pivot, table in-out and nested loop / merge / blockwise joins; packed CTE chunks; parallel delim-join distinct (q11 0.92×, q22 0.91×) |
| `COUNT(DISTINCT)` sinks | one growing table per thread (ClickBench q04 0.37×) | the radix sink strategy for DISTINCT tables (q04 0.64×) |
| Opening a database | every row group's column metadata read at open (ClickBench 200 MB, q00 8.2× DuckDB's memory) | metadata read when a column is first used (41 MB, q00 0.98×) |
| LIMIT | pipelines with a LIMIT ran on one thread | DuckDB's plan choice: a parallel streaming limit when the order does not matter, a batch `PhysicalLimit` over filtered scans and joins |
| Partial blocks | a block shared by several segments was read once per segment (a third of TPC-H q7's reads at one thread) | such blocks stay among the 16 last released blocks |
| Scan filter order | filters ran in column order; a join's bloom filter could hash every row before a cheap comparison | cost order and run-time swaps (DuckDB's `AdaptiveFilter`) |
| Row matching with NULLs | keys that may be NULL went through a generic predicate call per row | equality loops per selection kind with the stored validity byte and bit hoisted |
