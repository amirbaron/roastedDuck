# Work queue

Workstreams found while closing conformance failures. Execute sequentially: the
owner limits this workspace to one task and one build or test workload at a time.
The current goal is functional/storage/client C++ parity with the lower-memory
performance acceptance in `docs/performance-acceptance.md`, not independent speed and
memory parity. The native-name integration now compiles across workspace targets;
historical full-corpus and shell results still require final-source revalidation.
Each names the C++ it ports and the tests or checks that close it. Finished streams are
removed; the commit history records them.

Priority: the shell (3) first — the owner's top priority (2026-10-21):
it must be DuckDB's shell one to one, interactive editing included — then compression
(5) and the rest; the C API leftovers (2) when a client needs them.

## Public surface (M8)

2. **C API, the rest.** The integration checkpoint exports all 546 frozen duckdb.h
   functions plus `duckdb_adbc_init`. All 30 `test/api/capi` files pass at that snapshot
   (`scripts/capi-tests/run.sh`, as against the C++ library); final-source verification
   remains required.
   Left:
   - ADBC's 64-case frozen upstream suite passes on both engines with 3040 assertions;
     broader client/version/packaging matrices remain open. See `docs/adbc.md`.
   - Arrow per-type filter support and native producer filtering now exist. The C API
     raw-producer projection/filtering and dynamic-filter matrices still need explicit
     client verification; Python's producer tests alone do not close this gate.
   - Result Arrow settings snapshots are implemented and covered by the current signed
     30-file/155-case C API checkpoint; preserve that behavior in the final release gate.
   - Multi-statement PRAGMA extraction is a verified current functional gap. A real
     one-table import reports four extracted statements on C++ versus one on Rust;
     chained/repeated contexts are 6/8 versus 3/2. Active-transaction imports also differ.
     Port `StatementPreprocessor` transaction wrapping/invalidation and actual import
     expansion; preserve raw file/name bytes and validate registered filesystem access,
     atomic rollback and statement-object/ADBC behavior. Do not merely adjust a count.
     Frozen paired C API counterexamples and identical export fixtures:
     `extension-overloads-20261026/import-extract-before-1-comparison.json`.
     A paired registered-memory filesystem import also succeeds on frozen C++ but
     fails opening schema.sql locally on the preserved signed main-context Rust wheel.
     This before-fix package control is separate from current catalog package acceptance:
     `import-preprocessing-20261027/registered-import-before-1-index.json`.
     The implementation now passes 14 signed component tests for actual expansion,
     implicit rollback, active-transaction invalidation, snapshot/reset lifetimes and
     registered byte-valued imports. Fresh shipping C API/Python controls remain required
     before closure; see `docs/statement-preprocessing-parity.md`.
17. **Complete language clients.** Owner-confirmed scope (2026-10-25): Python, Go
   and Swift only. Preserve existing C API and ADBC functionality; other language
   clients are outside this goal. Inventory the versioned API surfaces and packages,
   implement missing bindings/adapters, then run upstream suites and real installation,
   query, interchange, UDF and resource-lifetime journeys against the Rust engine.
   C API smoke tests alone do not establish client parity; bundling the C++ engine is
   not completion. Track evidence and remaining gates in `docs/language-bindings.md`.

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
     Raw identifiers can also be created and queried in C++ while metadata conversion
     rejects them, so blanket validation of SQL input is incorrect. A shared checked
     byte-to-VARCHAR constructor is implemented for value construction;
     unchecked vector bytes and raw identifiers must remain distinct. Byte probes:
     `.context/artifacts/shell-bytes-20261023/`.
     That boundary passes 102 type/vector tests, all 30 C API files and 392 shell
     tests/three shared skips. A 65,823-input constructor matrix matches C++ exactly.
     Engine error messages and all native callback error carriers now own bytes:
     scalar, table, aggregate (including deferred errors), COPY, replacement and cast.
     SQL `error()`, native C API output and the shared cast/JSON transform/scanner path
     preserve bytes. NULs are sanitized before JSON conversion; JSON control escapes
     use uppercase hex. CAST retains source-column context; TRY_CAST retains the first
     nonempty error and NULL results. Aggregate state-size errors occur at physical
     preparation after optimization, including SQL PREPARE; pruned aggregates skip
     the callback and window aggregate size errors remain at execution. JSON-mode
     prepare no longer adds a caret to raw messages beginning with `{`.
     All 13,832 cases in `scripts/capi-error-bytes.py --family all` match C++, including
     all byte values/selected sequences, direct/prepared, plain/JSON, callback phases,
     deferred/empty-input errors, combine, pruning, SQL PREPARE, windows and recovery.
     The 35 function tests, workspace Clippy, 30 C API files, 765 shell cases (zero
     unexpected/five known), 392 shell tests/three skips and full 4,870-file corpus
     pass their gates. The corpus remains 4,781 pass, 15 accepted divergences and
     74 shared skips, with zero crashes/timeouts or ratchet regressions. Raw shell,
     parser/name bytes and legacy text wrappers are still open. Current artifacts:
     `.context/artifacts/callback-bytes-20261023/` (`first-path.txt`, `final-path.txt`);
     initial scalar/constructor evidence remains in `shell-bytes-20261023/`.
     Raw shell lines/history/SQL accumulation and dot-command tokenization now own
     bytes. Octal escapes retain invalid bytes; decoded NUL terminates one argument.
     Literal `.print` uses the byte callback interface, and raw command-name lookup
     and errors preserve bytes. The old `input-latin1-dot-print` known marker is removed.
     All 778 native byte comparisons match C++; 14 shell +23 common units, workspace
     Clippy, 768 shell cases (zero unexpected/four known) and 392 upstream shell tests
     with three shared skips pass. No full SQL sweep was repeated for this frontend
     step; the latest accepted full corpus is the callback-byte sweep above.
     Other metadata callbacks, invalid-byte error highlighting, argv and SQL/parser/name
     storage still have explicit text boundaries. No blanket UTF-8 SQL rejection was
     added. Probes/manifests: `.context/artifacts/shell-input-bytes-20261023/`.
     Its `raw-name-files/report.json` shows C++ can persist/reopen raw table, column,
     schema and type names; Rust rejects all four at UTF-8 string deserialization.
     Valid-UTF-8 controls match and all read-only hashes are unchanged. Continue with
     byte-owning typed identifiers across query/parser/catalog/serialization, using
     the binary deserializer's existing raw string-byte interface.
     Introduce an owned byte-string type with checked UTF-8 access (no implicit text
     dereference or lossy display), and a raw binary string codec before migrating
     generated AST/catalog name fields. Compare its numbered-property wire bytes
     against C++ for raw/empty/NUL/long strings. Keep strict String decoding for text
     fields; this codec foundation alone does not close the four raw-name file gaps.
     During the name migration, native formatting must also own bytes. Bound and
     parsed expression/query renderers must recursively use byte output rather than
     sending names through Rust Display. A byte-format helper may select a native
     renderer for byte-owning types and use ordinary Display for text/numeric types;
     ByteString itself must never gain a lossy Display or text dereference. Preserve
     numbered-property wire bytes, checked VARCHAR boundaries, keyword quoting and
     C++ formatting evidence while extending bindings and result names. Partial name
     migrations are not an accepted workspace or shell baseline.
   - Startup main/schema identities now match the frozen C++ values 20624/20622 across
     independent instances, and the shell database-list case matches literal bytes.
     Its obsolete known-difference waiver is removed. Later temporary-catalog,
     dependency-entry and registration-order identities remain an explicit audit; see
     `docs/catalog-oid-parity.md` and external
     `extension-overloads-20261026/public-catalog-comparison-1.json`.
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
6. **External file cache, the rest.** `ExternalFileCache` and `CachingFileSystem` are
   ported (`crates/function`: buffer-managed ranges, validation by version tag or
   modification time, `enable_external_file_cache`, `validate_external_file_cache`,
   `duckdb_external_file_cache()`), and the Parquet reader reads through them as
   DuckDB's does (the footer, prefetched row groups or columns, 256-byte page headers);
   the cache's ranges and memory match DuckDB's. Left:
   - `external_file_cache_eviction_queue` expects the scan of a table to evict the
     cached ranges: in DuckDB the materialized result (`COLUMN_DATA`) takes the memory.
     It passes once results are buffer-managed (4); a trial merge with that branch
     passes the whole directory.
   - In the prefetch mode, a read outside the prefetched ranges (page offsets outside
     their column chunk) is a direct read, where DuckDB prefetches up to 1 MB through
     the cache (`PREFETCH_FALLBACK_BUFFERSIZE`).
   - `CachingFileSystemWrapper` (the JSON reader, `read_text`, `read_blob`) caches
     remote files only: there is nothing to port until a remote file system exists.
15. **Parquet modular encryption, the rest.** Database encryption is ported
    (`crates/crypto` on RustCrypto; database files, the WAL and temporary files; files
    open in either engine for every cipher, encryption version and storage version, and
    the encryption tests pass without their `require httpfs`). The Parquet writer's
    `encryption_config` is not implemented (`parquet_encryption`,
    `parquet_encryption_tpch`, which `require httpfs`), and the reader decrypts with its
    own AES in `crates/parquet/src/crypto.rs`, which should move onto `crates/crypto`
    (hardware AES, constant time).

16. **Persisted ART reload, remaining paths.** Modern disk allocator metadata is now
    restored for plain and stored constraint indexes without a table scan. ART buffers
    load lazily, keep unread disk blocks alive, hold the file pin while allocating and
    copying, and unload after checkpoint writes as C++ does. The profiling regression
    exposed by table caching passes again without changing counters or expectations.
    The broader full sweep caught a concurrent publication race: deferred index writes
    were flushed after releasing the index lock. Serialization now flushes within the
    bound-index guard or unbound-storage mutex, before exposing the new disk locations.
    Validation: 55 storage unit tests; 441 passes/ten shared skips over 451 affected
    SQL files; both concurrent failures pass three targeted trials and the cohort;
    ten C++/Rust file cases across storage 1.0/1.2/1.5 and GCM/CTR,
    including cross-engine mutation/reopen and PK/UNIQUE/FK checks. Every file case
    checks zero ART buffer residency before first access. C++ encrypted fixture writes
    use its documented `force_mbedtls_unsafe` test switch because this reference build
    has read-only crypto; C++ reads use normal crypto and fixtures contain synthetic data.
    Workspace Clippy, 30 C API files and 392 shell tests/three shared skips pass on
    fresh signed public binaries. The 763-case shell differential has no unexpected
    difference (five known shell engine gaps remain under item 3).
    A subsequent full sweep on `506354f` plus the cache flag has 4781 passes,
    15 accepted failures and 74 shared skips, zero crashes/timeouts and zero regressions
    across the unchanged 4855-file ratchet. This includes fixes for snapshot-tail
    filtering and partial mutation undo: 80 storage tests, 100 original stress trials,
    20 diagnostic trials and 374 affected SQL passes/one shared skip all pass. Forty
    C API stress trials and explicit-transaction lock behavior also pass against C++.
    Evidence:
    `.context/artifacts/index-reload-20261022/`.
    Left: native expression-index binding and buffered WAL replay, native CREATE INDEX
    WAL buffers, pre-v0.10 root-pointer deserialization, and any missing/in-memory
    metadata still rebuilt as a fallback. These remain explicit parity gates; do not
    treat the fixed profiling test or modern-file checks as proof that all formats and
    reload paths are ported.

## Planner and execution

7. **Performance acceptance preserving the lower-memory baseline.** The owner
   confirmed on 2026-10-23 that roughly 15–20% longer execution with 60–70% less memory
   on TPC-H/ClickBench is acceptable. Preserve the uncached baseline; do not increase
   memory to copy C++ caching or close small speed gaps. The table-cache experiment is
   rejected, with its patch and evidence retained externally. Evaluate optimizations
   on both speed and memory, and report individual query outliers separately from
   averages. Finish `docs/performance-acceptance.md` on the final signed binaries.
   Remaining useful optimization candidates include cheaper reads without higher
   residency, aggregate lookups/resizes and join probes, subject to that policy.
8. **Logical plan serialization, the rest.** `json_serialize_plan` writes logical plans
   with DuckDB's serializers: `tools/codegen` generates property structs from
   `logical_operator.json`, `expression.json`, `nodes.json`, `table_filter.json` and
   `column_index.json`, and the hand-written parts (`LogicalGet`, functions and their
   bind data, `COPY TO`, lambdas) follow DuckDB's. Functions carry their catalog entry,
   original arguments and a `serialize` callback; binds that replace the function
   object clear the entry, like DuckDB's. Parquet scans and writes, STRUCT `date_part`
   (ICU's too), binning and lambdas write their bind data; CSV and JSON scans and
   exported aggregate states fail as DuckDB's do. What still differs:
   - `EXPORT DATABASE` is not serialized: rustdb's `LogicalExport` keeps what its
     operator needs instead of DuckDB's `CopyInfo` and `BoundExportData`, and unions the
     table copies two at a time.
   - Equal aggregates are shared when their functions have the same name; DuckDB
     compares the functions' callbacks, so `median(x)` and `quantile_cont(x, 0.5)` are
     one aggregate there.
   - A scan of encrypted Parquet files cannot be serialized: rustdb resolves the footer
     key's name to the key while binding.
   - Not comparable: `SELECT $1` and the optimized plans DuckDB fails to bind ("Failed
     to bind column reference ... inequal num bindings/types", e.g. TPC-H 2, 5, 7, 8, 9,
     12, 21 and `SUMMARIZE`) are DuckDB internal errors with stack addresses, and
     `now()` folds to the transaction's time.
9. **Physical operator logging, the rest.** Every `PhysicalOperatorLogType` site is
   ported (the hash join's `Finalize`, `Repartition` and `GetData`, `JoinHashTable`'s
   `Build`, the Parquet writer's `FlushRowGroup`, the reader's `ReadRowGroup` /
   `SkipRowGroup`), with the operator's type and `ParamsToString()`, and
   `debug_force_external` reaches the hash join, the sorts, the window merge sort tree
   and the temporary memory manager. Two `size`s differ: `Build` counts the build
   collection's chunk metadata as rustdb holds it (16 KiB where DuckDB's arena holds
   4 KiB), and `FlushRowGroup` estimates DuckDB's buffer (`ColumnDataCollection::
   SizeInBytes`: exact for fixed-size columns, strings and nested types approximate).
11. **ICU collation and sort keys — ported.** Merged as `2fa715c`. The runtime in
    `crates/icu/src/icu4c/collation*.rs` reads DuckDB's ICU 66 root data and tailorings
    from `crates/icu/data/icu66_collation.bin`; ICU4X is removed. The offline exporter
    (`scripts/icu-collation-data.sh`, `scripts/icu-collation-data.cpp`) uses DuckDB's
    embedded `icudt66l.dat`; no C++ runtime is shipped. Its upstream notice is retained
    in `crates/icu/LICENSE-ICU`. All 24 collation files pass, including
    `test_icu_extensive.test_slow` (`as`/`kok`/`mr`); five timezone files and plan
    serialization also pass. Fresh comparisons against C++ match all 130 collations'
    sort keys over 484,110 strings, plus locale keywords, names and BCP 47 tags.
    Evidence: `.context/artifacts/icu66-resume-20261022/`. The full sweep and two
    default-thread rechecks yield 4781 pass, 15 accepted divergences and 74 skips, with
    zero ratchet regressions. Rare ICU FCD iterator hangs intentionally terminate
    by collating the NFD form; these cannot be compared to a completed C++ key.
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
