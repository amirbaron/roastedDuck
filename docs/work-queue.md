# Work queue

Workstreams found while closing conformance failures, in the order they are scheduled
(at most two agents run at once). Each names the C++ it ports and the tests or checks
that close it. Finished streams are removed; the commit history records them.

## Public surface (M8)

1. **C API, core.** `libduckdb` exports 17 of the 547 `duckdb_*` functions of DuckDB
   1.5.6. Port `src/main/capi/` for results, values, prepared and pending statements,
   streaming, appender, data chunks, vectors, logical types, table description,
   configuration. Verify with DuckDB's own `test/api/capi/*.cpp` (Catch) compiled against
   rustdb's `libduckdb` and header (test code only; nothing C++ ships), and diff the
   exported symbol list against the C++ library.
2. **C API, extension surface.** Scalar, aggregate, table and copy functions, replacement
   scans, custom types, file system, logging, profiling, instance cache, catalog, the
   Arrow C data interface (`duckdb_arrow_*`, `arrow_scan`) and ADBC.
3. **CLI shell.** `crates/shell` is a stub that prints `a|b` rows; DuckDB's shell renders
   duckbox tables (also for `-c` and piped output), dot commands and modes. Port
   `tools/shell` with byte-identical rendering.

## Storage

4. **Optimistic writes.** Inserts keep every row in memory until the checkpoint and log
   them to the WAL; DuckDB's `OptimisticDataWriter` writes full row groups to blocks
   during the insert and logs only their pointers (`ROW_GROUP_DATA`), and estimates the
   WAL growth from those pointers. Affects peak memory of bulk loads and WAL size. Tests:
   `optimistic_write_not_trigger_checkpoint`, `wal_size_unaligned_appends`,
   `reclaim_space_insert_unique_idx_optimistic`, `buffer_managed_query_result` (an
   800 MB table created under a 100 MB limit without a temporary directory). Replaying
   `ROW_GROUP_DATA` entries is part of it: rustdb cannot replay a WAL in which DuckDB
   logged an optimistically written insert. Starts after the in-memory checkpoint
   work merges (shared code).
   **Concurrent checkpoints** belong to the same WAL work: rustdb's checkpoint holds the
   WAL lock, so commits wait for it, while DuckDB lets them continue into a new WAL and
   recovers from `.wal.checkpoint` plus `.wal` (the `concurrent_wal` tests).
5. **Compression parity.** ZSTD strings compress about 16% worse than DuckDB's
   (`zstd_compression_ratio`). The cause is `ruzstd`'s encoder, not its match level: on
   concatenated UUIDs it reaches 1.72× (1.76× emitting literals only, about 4.56 bits per
   hex character) where zstd reaches 1.95× at levels 1 and 3, so its Huffman literal
   coding is far from optimal. Needs a zstd block encoder of our own (length-limited
   Huffman literals, FSE-coded sequences with custom tables, a level-3 matcher); check
   `ruzstd`'s decoding speed against DuckDB's at the same time.
   Roaring and hugeint bitpacking produce different segment layouts than DuckDB
   (`roaring_compression_ratio`, `roaring_bool_compression_ratio`,
   `bitpacking_compression_ratio_hugeint`); compare files byte for byte with the C++
   writer.
6. **External file cache.** `CachingFileSystem`, `duckdb_external_file_cache()`,
   `validate_external_file_cache` (`external_file_cache_*` tests).

## Planner and execution

7. **Performance round 4.** Batch `LIMIT` for filtered scans, partitioned aggregate,
   struct projection pushdown, hash join probe, `COUNT(DISTINCT)`, small scans
   (`docs/perf-backlog.md`); the EXPLAIN tests that check these plans
   (`test_batch_limit_filters`, `partitioned_group_by`,
   `plan_struct_projection_pushdown`, `top_n_pruning`, `unnest_rewriter`,
   `deliver_child_columns`).
8. **Logical plan serialization** (`json_serialize_plan`).
9. **Physical operator logging** (`physical_operator_logging`).
10. **TPC-DS** (`test/sql/tpcds`, the `tpcds` extension).
11. **ICU collation data parity** (`test_icu_extensive`). Sorting a mixed sample under each
    of the 130 collations differs from DuckDB for 12: DuckDB applies tailorings that
    ICU4X's compiled data lacks for `as`, `haw`, `kl`, `kok`, `lkt`, `mr`, `se`, `smn`
    (e.g. Assamese sorts `ক্ষ` after `হ`), while for `nb`, `nn`, `nb_no` and `sr_me`
    DuckDB's trimmed ICU data has no tailoring (root order) and ICU4X applies Norwegian
    and Serbian rules. Generate the collation data from ICU 77's export data with
    `icu4x-datagen` for DuckDB's locale list and pin the root-order locales to root.
