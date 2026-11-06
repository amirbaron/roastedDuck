# DuckDB → Rust: porting plan

Target: a 100% Rust engine that is a 1:1 drop-in replacement for DuckDB **1.5.6**
(the frozen reference in `.context/duckdb-download/duckdb-1.5.6`).

## Requirements

1. **Same SQL results.** The whole DuckDB sqllogictest corpus (4,870 `.test` files,
   ~74k `query`/`statement` records) passes unchanged, error messages included.
2. **Same files.** The Rust engine opens databases written by C++ DuckDB, and
   C++ DuckDB opens databases written by the Rust engine (storage + WAL byte format).
3. **Same ABI.** It ships a `libduckdb` exporting the C API (`duckdb.h`, 547
   functions, including ADBC) and loads C-API extensions (`duckdb_extension.h`).
4. **Preserve the accepted speed–memory tradeoff.** The owner approved the measured
   lower-memory baseline on 2026-10-23: roughly 15–20% longer execution with 60–70%
   less memory on TPC-H and ClickBench is acceptable. Do not raise memory merely to
   match C++ caching or a small speed gap. Compare both metrics against that baseline,
   report individual query outliers, and document final performance acceptance in
   `docs/performance-acceptance.md`. Exact speed/memory parity is not required.
5. **Runs after every step.** Every merged change builds, the `rustdb` CLI and
   `libduckdb` run, and the conformance pass count never goes down.
6. **Independent review.** At every milestone, reviewer agents that did not write the
   code assess quality, performance and correctness. The milestone closes only when
   they all sign off (see "Independent review gate").

The DuckDB engine remains Rust. On 2026-11-01 the owner explicitly allowed
third-party native crypto, including OpenSSL and ring, superseding the original
artifact-wide prohibition on C/C++ and their build dependencies. The C++ DuckDB
engine is used only as a test and benchmark oracle, never as the shipped engine
or a runtime fallback. See [the handoff clarification](handoff.md).

## Scale (cloc, code lines only)

| Area | Lines | Notes |
|---|---:|---|
| `src/` core engine | ~363k | 1,380 `.cpp` + 1,342 headers |
| In-tree extensions | ~105k real code | ICU dir is 437k but mostly generated data tables |
| `third_party/` | ~400k | almost all replaced by crates (see below) |
| Biggest core subsystems | | execution/operator 31k, common/types 25k, storage/table 15k, binder 14k, scalar functions 14k, table functions 13k, compression 10k, C API 9k |

## Strategy: rewrite with an oracle, not FFI strangler

**Recommended:** a ground-up Rust implementation that mirrors DuckDB's architecture
module by module (same layering, same on-disk formats, same observable behaviour),
driven by the C++ engine as a differential oracle. Internals are free to be better
than the C++ wherever results and formats stay identical; that's where the speedups
come from.

**Not recommended:** swapping C++ modules for Rust ones behind FFI one at a time.
DuckDB's internals share `Vector`/`DataChunk` memory layouts, template-instantiated
kernels and virtual hierarchies across every layer. A fine-grained FFI seam means
marshalling vectors both ways and owning cross-language lifetimes in every operator,
which costs more than writing the Rust side outright.

**Not recommended:** building on DataFusion. Its type system, casting rules, NULL
ordering, function catalog and storage are different. Matching 74k DuckDB test
expectations means fighting it everywhere. Reuse leaf crates (arrow-rs C data
interface, parquet thrift types), not the engine.

DuckDB already serializes its intermediate forms, so we get coarse oracles for free:

| Seam | C++ oracle | Used to verify |
|---|---|---|
| Parse tree | `json_serialize_sql()` (json extension) | Rust parser + transformer |
| Logical plan | `logical_operator.json` serializer, `EXPLAIN` | binder + optimizer |
| Results | sqllogictest corpus + differential fuzzing | everything |
| Storage | `test/storage/bc/*.db` + round-trip both directions | storage, WAL, compression |
| Speed | `benchmark/` suites run on the C++ build | performance gates |

## Always runnable: walking skeleton + ratchet

The engine is end-to-end from the first milestone: `rustdb` CLI → parser → binder →
executor → result rendering, even when each stage only supports a sliver of SQL.
Later milestones widen the stages instead of building them in isolation.

- **Unsupported ≠ crash.** Anything not yet ported raises DuckDB's
  `Not implemented Error: <feature>`, never a panic. Fuzzing enforces no-panic.
- **Ratchet file.** `conformance/passing.txt` lists every test file that passes. CI fails
  if a listed file fails; newly passing files get added in the same PR. The count only
  goes up.
- **Perf ratchet.** `bench/baseline.json` stores Rust/C++ timings and peak memory per
  benchmark. CI fails on a regression beyond noise in either, on any benchmark that
  already runs.
- **Smoke run per PR.** Build release, run the CLI on a scripted session, load
  `libduckdb` from a tiny C program, run the ratchet set.

## Codegen from DuckDB's own specs

A large part of DuckDB is generated from JSON specs. Generate the Rust from the
same inputs instead of hand-porting the output:

- `src/include/duckdb/storage/serialization/*.json` (16 files) → Rust
  serializer/deserializer for the BinarySerializer format (field IDs, defaults,
  versioning via `src/storage/version_map.json`). This gives byte-compatible WAL,
  catalog and plan serialization.
- `src/common/settings.json` → settings registry.
- `extension/core_functions/**/functions.json` (20 files) → function registration stubs.
- `src/common/enums/metric_type.json` → profiler metrics.
- Bison parse tables from `third_party/libpg_query` → Rust constants (see M2).

## Third-party replacement map

| C/C++ dep | Rust replacement | Compatibility requirement |
|---|---|---|
| libpg_query (bison + flex) | Rust LALR driver over bison's extracted tables + hand-ported scanner | AST identical via `json_serialize_sql` |
| jemalloc | own arenas + evaluated pure-Rust allocator (see Performance) | none |
| re2 | `regex` | same linear-time model; check syntax-edge tests in `test/sql/function/string/regex*` |
| utf8proc | `unicode-normalization`, `unicode-segmentation` | grapheme/NFC results match |
| fmt | own fmtlib-syntax formatter | SQL `format()` uses fmtlib syntax; `printf()` uses printf syntax |
| yyjson | `serde_json` / `sonic-rs` | JSON text output byte-identical (number formatting) |
| zstd | `rustdb-zstd` (port of zstd 1.5.6's encoder and decoder) | frames identical to zstd's at levels -5 to 3 (`ZSTD_fast`, `ZSTD_dfast`); stronger levels write valid frames with the double-fast match finder; any frame without a dictionary decodes, whole or streamed |
| brotli / lz4 / snappy / miniz | `brotli`, `lz4_flex`, `snap`, `miniz_oxide` | frame-compatible |
| mbedtls | RustCrypto (`aes-gcm`, `sha2`, `hmac`), `rustls` | encrypted DB files readable both ways |
| httplib | `hyper` / `reqwest` + rustls | — |
| thrift + parquet | thrift types from `parquet` crate; own reader/writer into DuckDB vectors | port `extension/parquet` (22k) |
| fsst | `fsst-rs`, or port (1.2k) | **bit-exact** symbol tables for on-disk FSST |
| fastpforlib | port | **bit-exact** for bitpacking storage |
| hyperloglog | port | **bit-exact**: HLL sketches live in stored column stats |
| pcg | `rand_pcg` (same variant) | `setseed()` + `random()` sequences match tests |
| ICU (extension) | port of ICU 66's calendars and collation runtime (over DuckDB's ICU 66 collation data) + `jiff` (tz) | timezone/collation tests match; sort keys byte-identical |
| tpch dbgen | port (`crates/tpch`, DuckDB's embedded dbgen) | dbgen-identical output (verified byte for byte, SF 0.000001 to 3) |
| tpcds dsdgen | port | test data identical |
| jaro_winkler, skiplist, tdigest, pdqsort, ska_sort, vergesort | port or `std` sort | exact numeric results |
| concurrentqueue | `crossbeam` | — |
| catch | `cargo test` | — |

## C++ → Rust architecture mapping

- **Closed hierarchies → enums.** `ParsedExpression`, `Expression`, `LogicalOperator`,
  `TableRef`, `SQLStatement`, `LogicalType`.
- **Open hierarchies → traits.** Scalar/aggregate/table functions, `PhysicalOperator`,
  `FileSystem`, storage extensions, compression functions.
- **Operator state** (`GlobalSinkState`, `LocalSourceState`, ...) → associated types on
  the operator trait where the set is closed, `Box<dyn Any + Send>` only at extension
  boundaries.
- **Templates → generics + macros.** `UnaryExecutor`/`BinaryExecutor`/`AggregateExecutor`
  become generic kernels, monomorphised the same way.
- **Exceptions → `Result<T, Error>`.** `Error` carries DuckDB's `ExceptionType` and the
  message text verbatim; tests assert on error strings.
- **`Vector` / `DataChunk` / `string_t`** keep DuckDB's physical layout (16-byte
  inline/prefix strings, validity bitmasks, flat/constant/dictionary/sequence/FSST
  vectors, Arc-shared buffers). This is the main `unsafe` core, wrapped in safe
  typed accessors.
- **Buffer manager:** RAII pin guards over refcounted block handles (not borrow-checker
  lifetimes), eviction queue, temp-file spilling, memory limit.
- **Scheduler:** DuckDB's morsel-driven task and pipeline/event model on a dedicated
  thread pool. Not rayon, not async: blocked-task and interrupt semantics must match.

## Performance: C++ parity

Use measured gaps against C++ to choose engineering work. Improvements beyond parity
are optional. All changes stay internal (results, plans' semantics and file bytes stay
identical), and land only with a benchmark that demonstrates their benefit.

| Lever | Where |
|---|---|
| Monomorphised kernels; no virtual calls inside vector loops | expression executor, aggregates, comparisons |
| Fused expression evaluation for common shapes (filter + projection chains, `CASE`, arithmetic trees) | expression executor |
| Explicit SIMD with runtime CPU dispatch (`std::arch` + multiversioning) | filters, hashing, selection vectors, bit-unpacking, ALP/RLE/dictionary decode, string prefix compare |
| Better hash table probing (SwissTable-style group control bytes, salted pointers, prefetch) | hash join, hash aggregate, distinct |
| Allocation discipline: per-pipeline arenas, `DataChunk` reuse, no allocs in the hot path | everywhere; also replaces jemalloc |
| io_uring (Linux) and batched async prefetch; direct I/O for the block file | storage, Parquet/CSV scans, remote files |
| Late materialisation and zero-copy string handling across operators | scans, joins, sort |
| Scheduler: lower-overhead task handoff, better pipeline-level work stealing | parallel execution |
| Build: LTO, PGO (`cargo pgo`), BOLT on the release binary | release pipeline |

## Memory: preserve the lower-memory baseline

The owner's 2026-10-23 decision supersedes independent C++ speed and memory targets.
Preserve the measured uncached baseline and its lower peak RSS. Evaluate speed and
memory together, with correct results and unchanged storage/WAL formats. Persistent
table caching must not increase memory merely to imitate C++ or close a small speed gap.
The baseline, rejected cache evidence and final acceptance gates are recorded in
`docs/performance-acceptance.md`.

| Lever | Where |
|---|---|
| Stay compressed in flight: dictionary, FSST, RLE and constant vectors flow through filters, projections, joins and aggregates without being expanded | scans → operators |
| Compact row layout for hash tables and sort: 32-bit offsets within partitions, narrower group/key encodings chosen from column statistics, inline small strings | `TupleDataCollection` equivalent, join/aggregate/sort |
| Shared partitioned hash tables instead of a full per-thread table, where contention allows | parallel aggregate, distinct |
| String heaps deduplicated and shared across chunks; no copies for pass-through strings | aggregates, joins, materialisation |
| Own size-class arenas that return pages to the OS (`madvise`) as pipelines finish; no allocator-retained slack | replaces jemalloc everywhere |
| Buffer manager accounting covers every large allocation, so eviction and spilling act on true usage | buffer manager |
| Streaming instead of materialising where semantics allow (e.g. `ORDER BY … LIMIT`, window partitions, `COPY`) | physical planning |

Measured as peak RSS of the whole process (plus buffer-manager peak) for both engines at
the same `threads` and `memory_limit` settings.

## Measurement and final targets

**Benchmark harness (M0):** runs DuckDB's `benchmark/` suites (tpch, tpcds, h2oai,
clickbench, micro, csv, parquet, ingestion, imdb) on both engines on the same machine,
pinned cores, warm and cold runs, results checked equal before timings count. Records
wall time and peak memory for every benchmark.

**Final performance acceptance (M9):**

- Preserve the approved lower-memory tradeoff. Roughly 15–20% longer execution time
  with 60–70% less memory on TPC-H and ClickBench is acceptable; neither exact speed
  parity nor outperformance is a completion gate.
- Use the checked round-6 uncached results and preserved binaries as the comparison
  baseline. Report wall time, CPU time, peak RSS and available buffer/spill metrics
  per query at the same data, settings and thread counts. Correct results are required
  before timing a query can count.
- Report individual slow/memory-heavy queries and regressions against the baseline
  separately from suite averages. Repeat noisy outliers and explain serious regressions;
  improved averages cannot conceal them or justify memory increases for small speed gains.
- Rerun the final signed binaries, retain source/binary/data identities and raw evidence
  on the external SSD, and complete the acceptance report. The approved policy alone
  is not final-binary performance verification. Memory-limit and out-of-core correctness
  gates remain functional requirements.

## Independent review gate

At the end of every milestone, three reviewer agents run with fresh context. They did
not write the code and don't see the implementer's reasoning. Each is told to look for
reasons to reject, and returns a scorecard. A milestone closes only when all three
approve; every finding becomes a task before re-review.

1. **Code quality reviewer**
   - `rustfmt`, `clippy -D warnings` (pedantic set) clean.
   - Every `unsafe` block has a `// SAFETY:` comment and lives in an allowlisted module;
     Miri passes on the `types`/`vector` crates.
   - No `unwrap`/`expect`/`panic!` in library code paths.
   - Idiomatic Rust, not transliterated C++: enums over class hierarchies, no
     getter/setter soup, clear ownership, small modules with documented public items.
   - Rust code is shorter than the C++ it replaces, per module, unless justified.
2. **Performance reviewer**
   - Re-runs the benchmark harness on its own, comparing speed and peak memory against
     the C++ build.
   - Checks the milestone's gate and looks for regressions hidden by averages.
3. **Correctness reviewer**
   - Re-runs the conformance corpus, the storage round-trip suite and differential
     fuzzing for a fixed budget.
   - Spot-checks that no test was skipped, rewritten or loosened.

## Milestones

Each milestone ends runnable (CLI + `libduckdb`) and passes the review gate. The
dashboard reports pass rates per `test/sql/<dir>` against the Rust engine.

### M0 — Oracle, harness, walking skeleton
- Cargo workspace (crates: `types`, `parser`, `catalog`, `planner`, `optimizer`,
  `execution`, `storage`, `functions`, `main`, `capi`, `shell`, `ext-*`).
- Rust sqllogictest runner for DuckDB's dialect (`require`, `load`, `restart`, `loop`,
  `foreach`, `concurrentloop`, `mode`, `statement maybe`, result hashing, sort modes),
  running each file against C++ 1.5.6 (via `duckdb-rs`, dev-dependency only) and Rust.
- Oracle CLIs: dump `json_serialize_sql` and serialized logical plans for any statement.
- Benchmark harness against the C++ build; record the C++ baseline.
- Walking skeleton: `rustdb` CLI and `libduckdb` (`duckdb_open`/`duckdb_query`/result
  fetch) run `SELECT 42`, `SELECT 1 + 2, 'a'` end to end.
- CI: no-C/C++ guard, ratchet files, smoke run, clippy/fmt/Miri.
- Port ledger: C++ file → Rust module → status, for later upstream diffs.

**Gate:** runner passes 100% of the corpus against C++ (proves the harness); skeleton
runs; ratchet in place.

### M1 — Types, vectors, constant expressions
`LogicalType`, `Value`, `Vector`, `DataChunk`, `string_t`, validity, selection vectors,
hugeint/uhugeint, decimal, interval, date/time/timestamp math, the casting matrix
(`src/function/cast`), vector operations, serialization codegen. Exercised through the
SQL path with `SELECT <expr>` (no `FROM`).

**Gate:** cast and arithmetic tests that need no tables pass through the runner; vector
kernels at or faster than C++ in `benchmark/micro` and `benchmark/cast`.

### M2 — Parser
A port of DuckDB's default parser, not the PEG one. DuckDB parses with a bison LALR(1)
grammar (`third_party/libpg_query`, ~9.3k lines of `.y`) and a flex scanner, then
converts Postgres parse nodes into its AST (`src/parser/transform`, ~7k lines). The PEG
grammar in `extension/autocomplete` is opt-in upstream in 1.5.6 and only approximates
that language, so matching it would mean chasing differences in accepted syntax, operator
precedence and error positions statement by statement.

- **Tables, not a generator.** Bison's generated parse tables (`yytable`, `yycheck`,
  `yypact`, `yydefact`, `yypgoto`, ...) are extracted into Rust constants by a script,
  and a Rust port of bison's LALR driver runs them. Identical tables give identical
  parsing decisions and the same "syntax error at or near" token. Bison is only a
  dev-time tool for regenerating tables when the grammar changes.
- **Semantic actions** (the `yyparse` switch cases) are translated to Rust, building
  Postgres parse nodes defined in Rust.
- **Scanner:** a hand-written Rust port of `scan.l` (literal, operator, comment and
  identifier rules, plus the lookahead token filter in `parser.cpp`).
- **Transformer:** a 1:1 port of `src/parser/transform` producing the generated AST.

**Gate:** for every statement in the corpus, Rust AST JSON == C++ `json_serialize_sql`,
and every parse error has identical text; parse throughput at parity with C++.

### M3 — Single-threaded in-memory slice
In-memory catalog, binder for `SELECT`/`CREATE TABLE`/`INSERT`, expression executor,
operators: scan, filter, projection, hash aggregate, order, limit, hash/nested-loop join.

**Gate:** `select/`, `projection/`, `filter/`, `limit/`, `order/`, `types/` (in-memory
subsets) green; single-threaded micro benchmarks at parity with C++ with 1 thread.

### M4 — Full SQL surface
Complete binder (subquery decorrelation, CTEs incl. recursive/`USING KEY`, `PIVOT`,
`MERGE`, `QUALIFY`, lambdas, macros), the full optimizer (DPhyp join order, filter
pushdown, statistics propagation, TopN, CSE, late materialisation...), the whole
function library (`src/function` + `extension/core_functions`), window functions.

From here the work splits into parallel tracks: functions, optimizer, and
window/aggregate each own separate test directories.

**Gate:** `function/`, `aggregate/`, `window/`, `subquery/`, `cte/`, `join/`, `pivot/`,
`merge/` green; `EXPLAIN` plan shapes match C++ on TPC-H/TPC-DS queries.

### M5 — Parallel and out-of-core execution
Task scheduler, pipelines/events, partitioned hash join and aggregate, the sort in
`common/sort`, buffer manager, temp-file spilling, memory limits, progress bar,
profiler. Most of the performance levers land here.

**Gate:** `parallelism/`, `outofcore/` green; TPC-H/TPC-DS SF1–SF10 results identical;
in-memory TPC-H, h2oai and ClickBench have checked results and documented speed–memory
acceptance against the preserved baseline, including individual query outliers.

### M6 — Persistent storage, byte-compatible
Single-file block manager, metadata blocks, row groups, column segments, every
compression codec (uncompressed, RLE, bitpacking, dictionary, dict_fsst, FSST, ALP,
ALP-RD, chimp/patas read path, roaring validity, zstd), statistics incl. HLL, WAL,
checkpointing, MVCC (undo buffers, row versions), ART indexes, encryption,
`ATTACH`/multi-database.

**Gate:** opens every `test/storage/bc/*.db`; C++ opens every Rust-written DB and WAL
(round-trip suite); crash/WAL-replay tests; `storage/`, `transactions/`, `index/`,
`attach/`, `upsert/`, `alter/` green; ingestion and on-disk TPC-H results match, with
performance evaluated under the baseline policy.

### M7 — Formats and in-tree extensions
CSV sniffer and reader/writer (`copy/` is the second-largest test dir at 539 files),
Parquet, JSON, ICU, autocomplete, tpch/tpcds, httpfs-equivalent remote file system,
secrets, delta (`delta-kernel-rs` is already Rust).

**Gate:** `copy/`, `json/`, `timezone/`, `secrets/`, extension test dirs green;
`data/parquet-testing` read/write round-trips; CSV/Parquet benchmark correctness and
speed–memory results documented under the same performance acceptance policy.

### M8 — Public surface
- `libduckdb` cdylib with the full C API and C extension API.
- CLI shell (`tools/shell`, 15k lines); box/table rendering must match byte for byte.
- Arrow import/export via the Arrow C data interface (arrow-rs), ADBC driver.
- Native Rust API.
- Complete Python bindings first, then all primary language clients plus C#/.NET,
  Swift and Julia, as confirmed by the owner on 2026-10-23. Python and R need real
  bindings to this engine; their former exclusion is removed. The client/package
  inventory and per-language completion gates are in `docs/language-bindings.md`.

**Gate:** `test/api` equivalents green; the documented API coverage, upstream suites,
installation/packaging and resource/data-interchange journeys pass for every selected
client against the Rust engine. A C API smoke test is not full language-binding parity.
No client may silently use the C++ DuckDB engine to satisfy this gate.

### M9 — Parity sign-off
100% corpus green, including `.test_slow`; differential fuzzing (sqlsmith/SQLancer-style
generators, oracle = C++) clean over a fixed budget; complete client binding gates;
documented final performance acceptance preserving the approved lower-memory baseline;
CI's no-C/C++ guard green; final full review gate.

## What cannot be preserved

- **C++-API extensions** (most community extensions today) link against DuckDB's C++
  internals and cannot load into a Rust engine. Only C-API extensions work. Popular ones
  get ported as first-party Rust extensions.
Python/R bindings are required under M8. Existing upstream packages that use C++
internals cannot be treated as compatible merely because the C ABI is available;
they must be ported or replaced with complete bindings to the Rust engine.

## Keeping up with upstream

DuckDB releases often. After parity on 1.5.6, each new release is ported as a diff:
walk the upstream commit range, use the port ledger to map touched C++ files to Rust
modules, regenerate codegen outputs, then re-run the new release's corpus against both
engines.

## Working model for parallel agents

The unit of work is one C++ directory or file plus the test directory that exercises it.
Loop: read the C++ source, write the Rust, run that test directory against both engines,
diff results and timings, update the ratchet and port ledger. After M3 the dependency
graph is wide enough for many tracks at once (functions, codecs, CSV, Parquet, ICU,
optimizer rules, C API), which fits Conductor workspaces.
