# Work queue

Workstreams found while closing conformance failures (at most two agents run at once).
Each names the C++ it ports and the tests or checks that close it. Finished streams are
removed; the commit history records them.

Order of the next agents: the shell (3) first — the owner's top priority (2026-10-21):
it must be DuckDB's shell one to one, interactive editing included — then compression
(5) and the rest; the C API leftovers (2) when a client needs them.

## Public surface (M8)

2. **C API, the rest.** The C API exports 546 of DuckDB's 547 functions, and all 30
   `test/api/capi` files pass (`scripts/capi-tests/run.sh`, as against the C++ library).
   Left:
   - ADBC (`duckdb_adbc_init`, `src/common/adbc`). Its test, `test/api/adbc/test_adbc.cpp`,
     needs `arrow_test_helper.hpp`, nanoarrow and the ADBC driver manager in the harness.
   - Arrow filter pushdown by column type (`ArrowPushdownType`): `arrow_scan` hands
     filters on every type to the producer, as `TableFunction` has no per-type hook.
   - `duckdb_result_get_arrow_options` reads the connection's settings when called, not
     those the result was created with.
   - `parse_statements` hands a PRAGMA's expansion of several statements out as one
     multi-statement, where DuckDB's `StatementPreprocessor` adds `BEGIN`/`COMMIT`
     statements around them, so `duckdb_extract_statements` counts fewer statements.
3. **CLI shell, the engine's part** (top priority). `crates/shell` ports `tools/shell`
   one to one, linenoise included (`crates/shell/src/linenoise`, a module of the shell):
   the 392 tests of `tools/shell/tests` that pass on the C++ CLI pass on rustdb
   (`scripts/shell-tests.sh`), and interactive sessions (editing keys, history and its
   search, completion, highlighting, multi-line editing, the pager, `.edit`) match the
   C++ CLI byte for byte under a pty. What still differs comes from the engine:
   - `errors_as_json` objects carry `error_subtype` and `position` only; DuckDB adds the
     exception's other extra info (`name`, `candidates`, `type`, `catalog`, `schema`,
     `stack_trace`, ...) in the iteration order of libc++'s `unordered_map`.
   - Internal errors have no `Stack Trace:` section.
   - `pragma database_size` reports a `memory_usage` of 0 bytes where DuckDB counts the
     buffer of the shell's last result (256 KiB): results are not buffer-managed (4).

## Storage

4. **Bulk-load memory, the rest.** Optimistic writes and concurrent checkpoints are
   ported. Three pieces remain:
   - Query results are a `Vec<DataChunk>`, not a buffer-managed
     `ColumnDataCollection`, so `FROM range(...)` over a large table does not run out
     of memory under a small `memory_limit` the way DuckDB does
     (`buffer_managed_query_result`).
   - `PhysicalBatchInsert` has no `BatchMemoryManager`: batches are not throttled
     against the memory limit.
   - `auto_checkpoint_skip_wal_threshold` is accepted, but a commit that triggers a
     checkpoint still writes its WAL entries.
   - Block numbering of optimistic writes: a `CREATE TABLE AS` whose row group has both
     small (partial-block) and full-block segments (e.g. a STRUCT with long strings) puts
     the partial block before the full blocks; DuckDB numbers it after them. Offsets
     match, block ids differ (compare `pragma_storage_info` of both engines).
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
15. **Database encryption** (`dict_fsst_expression_filter`, which attaches a database
    with `ENCRYPTION_KEY`; the other encryption tests `require httpfs` and are skipped
    on both engines). DuckDB encrypts blocks, the WAL and temporary files with
    AES-256-GCM (or CTR) through mbedtls when httpfs is not loaded
    (`EncryptionEngine`, `StorageManager` encryption options, the main header's
    encryption fields). Port it byte-compatibly with a pure-Rust AES-GCM, and check
    files both ways against the C++ CLI.
16. **`vacuum_rebuild_indexes`** (`vacuum_rebuild_indexes_attach.test_slow`): with the
    ATTACH option (or the setting) set, a checkpoint vacuums row groups of tables with
    ART indexes, drops empty row groups and rebuilds the indexes afterwards
    (`RowGroupCollection` vacuum state `can_rebuild_indexes`, the vacuum lock in
    `table_scan.cpp`); rustdb keeps row ids fixed whenever a table has indexes.

## Planner and execution

7. **Performance round 5.** The aggregate hash table's lookups and resizes, re-reading
   table blocks, the hash join probe, partitioned aggregate, struct projection pushdown,
   Top-N row group reordering (`RowGroupPruner`, `RowGroupReorderer`)
   (`docs/perf-backlog.md`); the EXPLAIN tests that check these plans
   (`partitioned_group_by`, `plan_struct_projection_pushdown`,
   `struct_projection_pushdown_in_storage`, `deliver_child_columns`, `top_n_pruning`,
   `test_top_n_nulls_row_group_pruner_extensive`, `unnest_rewriter`).
8. **Logical plan serialization** (`json_serialize_plan`).
9. **Physical operator logging** (`physical_operator_logging`): the
   `PhysicalOperator` log type is registered, but nothing writes it. DuckDB logs from
   the hash join (`Finalize` with `external`, `Repartition`, `GetData` with
   `total_probe_matches`), `JoinHashTable::Build`, the Parquet writer
   (`FlushRowGroup`) and reader (`ReadRowGroup` / `SkipRowGroup`), each with the
   operator's type and `ParamsToString()`. The test also needs
   `debug_force_external` to force the external hash join (16 partition builds with 4
   radix bits).
10. **TPC-DS** (`test/sql/tpcds`, the `tpcds` extension).
11. **ICU collation of `as`, `kok` and `mr`** (`test_icu_extensive`). DuckDB embeds ICU
    66's data; rustdb matches it for 127 of the 130 collations (tailorings ICU4X's
    compiled data lacks come from `crates/icu/data/collation.postcard`, locales ICU 66
    does not tailor use root). Assamese, Konkani and Marathi still differ: ICU 66 sorts
    conjuncts such as `ক্ষ` and `क्ष` after `হ`/`ह`, newer CLDR does not, and
    `icu4x-datagen` cannot read ICU export data before its own format (the
    `release-71-1` export is a 404). Needs ICU4X data built from ICU 66's tailoring rules.
12. **Out-of-core IEJoin, AsOf join and window** (`out_of_core_ie_join`,
    `out_of_core_asof_join`, `out_of_core_window`): over TPC-H SF1 `lineitem` under a
    300 MiB limit at 1, 2 and 8 threads rustdb runs out of memory ("failed to pin
    block") where DuckDB spills. Their sinks keep the sorted input pinned; DuckDB's
    sort into buffer-managed, spillable runs (`PhysicalRangeJoin` global sort state,
    `WindowHashGroup`), and the IEJoin/AsOf sinks finalize in two events
    (`AddFinishEvent`, see `docs/perf-backlog.md`).
13. **VARIANT extract pushdown into shredded columns.** VARIANT columns are stored,
    shredded and unshredded like DuckDB's; a scan of `col.a::INT` still unshreds whole
    values and extracts afterwards. DuckDB scans the typed column of a fully shredded
    field directly (`VariantColumnData::PushdownShreddedFieldExtract`,
    `VariantStats::PushdownExtract`), which needs the storage `PushdownExtract` of struct
    projection pushdown (7) first. Results are unaffected.
14. **Buffer eviction parity under tight limits** (`max_swap_space_error.test`, failing 4
    in 10 runs). After `CREATE TABLE t2 AS SELECT random() FROM range(200000)` under
    `memory_limit='1024KiB'`, two threads and a 4 MB swap limit, rustdb keeps 1,067,008
    bytes of IN_MEMORY_TABLE resident (above the limit) and 1.31 MB on disk, where
    DuckDB keeps 542,720 bytes resident and 1.84 MB on disk. With 1M rows, rustdb then
    holds three pinned blocks (804 KiB) and fails to allocate the fourth ("could not
    allocate block") before reaching the swap limit DuckDB reports ("failed to offload
    data block"). Compare what the insert path pins per thread and when DuckDB unloads
    transient segments.
