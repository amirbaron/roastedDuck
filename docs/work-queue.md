# Work queue

Workstreams found while closing conformance failures (at most two agents run at once).
Each names the C++ it ports and the tests or checks that close it. Finished streams are
removed; the commit history records them.

Order of the next agents: VARIANT storage (13) once the running WAL (4) or performance
(7) agent finishes, then the C API extension surface (2), the shell (3), compression (5)
and the rest.

## Public surface (M8)

2. **C API, extension surface.** The core C API exports 334 of DuckDB's 547 functions
   and passes DuckDB's core `test/api/capi` files (`scripts/capi-tests/run.sh`). Missing:
   scalar, aggregate, table, cast and copy functions, replacement scans, custom types,
   file system, catalog, config options, logging, profiling, instance cache, the
   threading API (`duckdb_execute_tasks*`), the Arrow C data interface
   (`duckdb_arrow_*`, `arrow_scan`) and ADBC; also a PRAGMA function catalog (the
   extract-statements PRAGMA case of `test_capi_extract`). Typed appends go through
   `Value`, so an overflowing DECIMAL append errors and invalid UTF-8 is rejected where
   DuckDB writes them.
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
    and Serbian rules. DuckDB embeds ICU 66.1's data (`extension/icu`, CLDR 36), whose
    `nb` has no `no` fallback for collation, while ICU4X's data is from a much newer
    CLDR. Generate collation data with `icu4x-datagen` for DuckDB's locale list, then
    compare every collation with the C++ CLI on a multi-script sample and pin the ones
    where ICU 66 differs (root order for `nb`, `nn`, `nb_no`, `sr_me`).
12. **`ALTER DATABASE ... SET ALIAS TO` / `RENAME TO`** (`test_alter_database_rename`):
    DuckDB re-keys the attached database (`DatabaseManager::RenameDatabase`,
    `AttachedDatabase::SetName`) and the catalog name follows it. rustdb copies the name
    into `Catalog`, every `CatalogSet` and `AttachedDatabase`, so it has to become shared
    and mutable first (about 50 call sites); schedule when no other agent edits the
    catalog crate.
13. **VARIANT storage and statistics.** Shredding VARIANT columns in storage
    (`shredding_state: SHREDDED`, `typed_value_type`, `fully_shredded` in `stats()`),
    statistics merging, empty keys, omitting untyped values, the storage-version check
    (`variant_storage_version` expects an error), `variant_extract` with `TRY_CAST`
    struct order, and the Parquet VARIANT writer and its statistics. `COPY (select
    [123::VARIANT]) TO '...parquet'` panics today (a VARCHAR vector read as a list).
    Tests: `test/sql/storage/types/variant/*`, `test/parquet/variant/*`,
    `shredded_variant_stats`, `variant_extract_try_cast`.
