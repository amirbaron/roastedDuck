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
   C++ CLI byte for byte under a pty. `scripts/shell-diff.py` compares both binaries on
   the cases of `scripts/shell-diff/*.toml` (every mode and type, duckbox layouts, dot
   commands, options, errors, stream interleaving, Ctrl-C, pty sessions): all match but
   the ones marked `known`, listed below. Errors carry DuckDB's
   extra info (`error_subtype`, `position`, `name`, `type`, `candidates`, `catalog`,
   `schema`, `call`) for missing entries, existing entries, missing columns, unmatched
   functions and syntax errors, and `errors_as_json` lists it in DuckDB's order: libc++'s
   `unordered_map`, rebuilt each time DuckDB catches the exception into an `ErrorData`
   (`Error::with_extra_info_round_trip`, `Error::caught`). What still differs:
   - Internal errors have no `Stack Trace:` section. DuckDB's `Exception::ToJSON` adds
     the `backtrace()` addresses of an INTERNAL exception as `stack_trace_pointers`, and
     `ErrorData::ConstructFinalMessage` appends them symbolized (`StackTrace::
     ResolveStacktraceSymbols`): its own C++ frames, which rustdb cannot print. A
     section of rustdb's frames needs a backtrace captured in `Error::internal` and
     formatted like DuckDB's; its bytes would still differ.
   - The shell reads input and arguments as UTF-8, invalid bytes becoming U+FFFD;
     DuckDB passes the bytes on (a string constant then fails with "Invalid unicode
     (byte sequence mismatch) detected in value construction", `.print` writes them as
     they are). Needs byte strings from the shell's input down to the parser.
   - Functions rustdb lacks, which `duckdb_functions()`, `PRAGMA functions` and the
     completion lists show: `icu_collate_<locale>` for each ICU collation (DuckDB's ICU
     registers every collation as a scalar function too), `seq_scan`,
     `duckdb_secret_types`, `duckdb_table_sample`, `duckdb_coordinate_systems`,
     `duckdb_external_file_cache` (6), `json_serialize_plan` (8), `tpcds`, `dsdgen` (10).
   - Object ids: the oid of a database (`PRAGMA database_list`'s `seq`) counts the
     catalog entries created before it, more in DuckDB.
   - "Did you mean" for a type name: DuckDB suggests among the default type entries it
     created so far (`DefaultTypeGenerator` creates them on first use, e.g. `INTEGER`
     after a table with an `INT` column, so `INTEGR` gets `main.INTEGER`); rustdb's
     builtin types are not catalog entries and get no suggestion.
   - A `memory_limit` far below an aggregate's state (`SELECT list(i) FROM range(1e7)`
     under `1MB`) fails in DuckDB and succeeds in rustdb (4).

## Storage

4. **Bulk-load memory, the rest.** Optimistic writes and concurrent checkpoints are
   ported, and materialized query results are buffer-managed like DuckDB's
   (`QueryResultMemoryType::BUFFER_MANAGED`, which the shell and the sqllogictest runner
   ask for; C API results stay on the heap, as DuckDB's `Connection::Query` keeps them):
   the result collectors append to a `ColumnDataCollection` (`crates/row/src/column_data`,
   DuckDB's segments, allocator and copy functions, nested types included, block for
   block), which spills or runs out of memory under a small `memory_limit` as DuckDB's does
   (`buffer_managed_query_result`) and which `pragma database_size` counts for the
   shell's last result. `PhysicalBatchInsert` throttles its unflushed batches with
   DuckDB's `BatchMemoryManager` (threads ahead of the lowest batch block while the
   batches take the memory it obtained; `MaxThreads` caps the threads by it). A commit
   that checkpoints and is estimated at `auto_checkpoint_skip_wal_threshold` bytes or
   more does not write its WAL entries: it takes the checkpoint lock before it commits,
   holds the WAL lock until the checkpoint ends, and fails if the checkpoint does
   (`skip_wal_write_due_to_checkpoint`). Left:
   - `PhysicalBatchCopyToFile` buffers its raw batches without DuckDB's
     `BatchMemoryManager`, so an ordered `COPY ... TO` of a large table does not
     throttle the threads ahead of the lowest batch.
5. **Compression parity, the rest.** ZSTD, roaring and hugeint bitpacking segments are
   DuckDB's (`zstd_compression_ratio`, `roaring_compression_ratio`,
   `roaring_bool_compression_ratio`, `bitpacking_compression_ratio_hugeint` pass;
   `crates/zstd` ports zstd 1.5.6's encoder and decoder, partial blocks are shared as
   `PartialBlockManager` shares them). Left:
   - zstd's greedy, lazy and binary-tree match finders: levels that pick them (5 and up,
     4 on inputs of 16 KiB or less and of 128–256 KiB) compress with the double-fast
     finder, so their frames are valid but not zstd's (Parquet `COMPRESSION_LEVEL` above
     3, spill files at levels 4 and 5).
   - FSST segments of the same strings hold slightly different counts than DuckDB's
     (5878 against 5877 strings in a 256 KiB segment): the symbol tables differ.
   - zstd throughput is 0.88–1.0× DuckDB's (`docs/perf-backlog.md`).
6. **External file cache.** `CachingFileSystem`, `duckdb_external_file_cache()`,
   `validate_external_file_cache` (`external_file_cache_*` tests).
15. **Database encryption** (`dict_fsst_expression_filter`, which attaches a database
    with `ENCRYPTION_KEY`; the other encryption tests `require httpfs` and are skipped
    on both engines). DuckDB encrypts blocks, the WAL and temporary files with
    AES-256-GCM (or CTR) through mbedtls when httpfs is not loaded
    (`EncryptionEngine`, `StorageManager` encryption options, the main header's
    encryption fields). Port it byte-compatibly with a pure-Rust AES-GCM, and check
    files both ways against the C++ CLI.

## Planner and execution

7. **Performance round 6.** Round 5 (`docs/perf-backlog.md`) ported the Top-N row group
   order, the partitioned aggregate and the UnnestRewriter's `unnest` pattern, and showed
   that caching table blocks like DuckDB makes rustdb's memory DuckDB's. Left: cheaper
   table block re-reads (caching is ruled out by the memory target), the aggregate hash
   table's lookups and resizes, and the hash join probe. Struct projection pushdown is
   ported (13).
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
    The sort keys `icu_collate_<locale>` and `icu_sort_key` return (hex) are ICU4X's,
    not ICU 66's bytes (`icu_collate_de('Äpfel')`: `2B4935…` vs `2A4834…`): orders
    agree, the strings differ. Matching them needs ICU4C's sort key format
    (`CollationKeys::writeSortKeyUpToQuaternary`) over the same collation elements.
13. **Struct and VARIANT extract pushdown, the rest.** The pushdown is ported one to one:
    `RemoveUnusedColumns` registers `struct_extract` / `variant_extract` chains (under a
    cast) on scanned columns, scans read only the fields used (`ColumnIndex` children;
    the scan's `Projections` list them; Parquet reads only those fields), and a table
    scan of a STRUCT or VARIANT column emits each extracted field as a column of its own
    (`PUSHDOWN_EXTRACT`: `StructColumnData` reads the field into the result, with a
    pushed-down cast; `VariantColumnData::PushdownShreddedFieldExtract` reads a fully
    shredded field's typed column, else unshreds, extracts and casts). `variant/tpch_sf1`
    passes in 33 s (DuckDB's `unittest`: 89 s). DuckDB's quirks come along: a filter on
    a field disables the pushdown for the whole column; `FindShreddedColumnInternal`
    stops one path element short, so a single-field path (`v.a::INT`) never reads the
    typed column, and `v.a.b::INT` over a primitive `a` returns `a`'s values where the
    extract should be NULL (C++ 1.5.6 does the same); a typed and an untyped path of the
    same field in one query (`v.a.b::INT` with `v.a::INT`) raise DuckDB's "This path
    wasn't found in the registered paths" internal error. Left:
    - A VARIANT nested in a STRUCT has no column statistics of its own here (DuckDB asks
      its parent, `GetStatisticsRef`), so its shredded fields are never read directly:
      such scans unshred (same results).
    - Zone maps of pushed-down fields: a filter on such a column (only a join's dynamic
      filter can be one) does not prune row groups by the field's statistics
      (`ColumnData::CheckZonemap` with `PushdownExtract`).
    - Parquet reads only the needed struct fields when the file's column has the
      requested type; with schema evolution (a cast per file) it reads the column whole,
      where `MultiFileColumnMapper` maps the fields.
    - `hash()` of a VARIANT differs from DuckDB's (also for unshredded values, so not a
      layout difference).
14. **Buffer eviction parity under tight limits, the rest.** `max_swap_space_error.test`
    passes (20 of 20 runs, alone and beside a directory run): an append that starts a new
    row group unpins the full row group's segments before it allocates the new ones, as
    DuckDB's `RowGroup::InitializeAppend` does, so a 1M-row insert under a 1 MiB limit
    spills until the swap limit ("failed to offload data block") instead of running out of
    pinnable memory. After `CREATE TABLE t2 AS SELECT random() FROM range(200000)` under
    that limit rustdb still keeps one 256 KiB block more resident than DuckDB (804,864
    against 542,720 bytes with a buffer-managed result, 1,067,008 against 804,864 with a
    streaming one): DuckDB charges its pipelines' chunks to the buffer pool
    (`BufferAllocator`, tag `ALLOCATOR`), rustdb's vectors are plain heap memory.
