# rustdb architecture and conventions

How the port is put together and the rules every change follows. The plan is in
[`porting-plan.md`](porting-plan.md).

## Crates

Dependencies point downwards only.

| Crate | Package | DuckDB counterpart |
|---|---|---|
| `crates/common` | `rustdb-common` | `common/exception`, `ErrorData`, constants |
| `crates/types` | `rustdb-types` | `common/types` (LogicalType, Value, Vector, DataChunk), vector kernels |
| `crates/serde` | `rustdb-serde` | `common/serializer` (binary + JSON) |
| `crates/ast` | `rustdb-ast` | `parser/*` node classes (generated from serialization specs) |
| `crates/pg-parser` | `rustdb-pg-parser` | `third_party/libpg_query` |
| `crates/buffer` | `rustdb-buffer` | `storage/buffer`, `StandardBufferManager`, temporary files, `TemporaryMemoryManager`, `ArenaAllocator` |
| `crates/row` | `rustdb-row` | `common/types/row` (TupleDataCollection, partitioning, row matcher, row aggregates), `common/sort` |
| `crates/function` | `rustdb-function` | `function/*` definitions (ScalarFunction, cast functions, ...) |
| `crates/functions` | `rustdb-functions` | builtin function library (`function/scalar`, `core_functions`, ...) |
| `crates/catalog` | `rustdb-catalog` | `catalog/*`, catalog part of `transaction/*` |
| `crates/planner` | `rustdb-planner` | `planner/*` (binder, bound expressions, logical operators) |
| `crates/execution` | `rustdb-execution` | `execution/*` (physical operators, pipelines, executor) |
| `crates/rustdb` | `rustdb` | `main/*` (DuckDB, Connection, ClientContext, results) |
| `crates/capi` | `rustdb-capi` (`libduckdb`) | `main/capi` |
| `crates/shell` | `rustdb-shell` (`rustdb` binary) | `tools/shell` |
| `tools/slt` | conformance runner | `test/sqlite` |
| `tools/bench` | benchmark harness | `benchmark/` |

New subsystems (storage, optimizer, parquet, json, icu, ...) get their own crate when they
land, following the same table.

## Porting rules

- **Behaviour is defined by DuckDB 1.5.6.** Results, error text, file bytes and C API
  behaviour must match. When unsure, run the reference CLI
  (`.context/duckdb-download/duckdb-1.5.6/build/release/duckdb`) and compare.
- **Port the algorithm, not the syntax.** Keep DuckDB's structure (same operators, same
  phases, same names where they help navigation) but write idiomatic Rust: enums for
  closed sets, traits for open sets, `Result` instead of exceptions, ownership instead of
  raw pointers.
- **Error messages are copied verbatim**, including punctuation. `rustdb_common::Error`
  renders `"<Kind> Error: <message>"` exactly like `ErrorData`.
- **Unimplemented is an error, never a crash or a wrong answer.** Return
  `Error::not_implemented(..)` for anything not ported yet.
- Generated code (from DuckDB's specs or tables) is checked in, marked as generated, and
  reproducible with a documented command.

## Code rules

- `cargo fmt --all` and `cargo clippy --workspace --all-targets` clean under the workspace
  lints (pedantic). No `unwrap`/`expect`/`panic!` in library code; tests may use them.
- `unsafe` only where the data layout requires it (vector buffers, `StringT`, the C API).
  Every block has a `// SAFETY:` comment. Wrap it in safe APIs inside the owning module.
- Public items have doc comments. Comments explain *why*; they don't narrate the code.
- No dependency that compiles C/C++ (no `cc`, `cmake`, `*-sys` crates).

## Vectors and kernels

- `Vector` has DuckDB's representations: flat, constant, dictionary (child always flat),
  sequence, plus list/struct/array children and shared string heaps.
- Kernels read inputs through `Vector::unified::<T>()` and should use the helpers in
  `rustdb_types::kernels` (or equivalent fast paths): constant inputs produce constant
  outputs, and flat inputs without NULLs take a tight loop. Never convert row by row
  through `Value` in hot paths.
- Strings returned by a kernel that point into an input's heap need
  `result.add_heap_references(input)`; otherwise copy them with `add_string`.

## Operators

- A physical operator implements `PhysicalOperator` plus one or more of the typed traits
  `Source`, `Operator`, `Sink`, each with `Global` (shared, `Sync`) and `Local`
  (per-thread) state types. The executor sees the type-erased versions.
- Pipelines break at sinks (`parallel::meta_pipeline`, DuckDB's `MetaPipeline` /
  `BuildPipelines`). A sink's finalized global state is passed to the same operator's
  source and operator roles.
- Execution follows DuckDB's `src/parallel`: every meta pipeline gets an event stack
  (initialize → pipeline → prepare finish → finish → complete) linked by the pipelines'
  dependencies; events queue tasks on the database's `TaskScheduler` (`SET threads`), and
  the thread running the query works on its own tasks until the query completes.
- An operator opts into parallelism with `parallel_source` / `parallel_operator` /
  `parallel_sink` (default: sequential) and states its row order with `source_order` /
  `operator_order` / `sink_order_dependent`. Sources that number their output in batches
  (`supports_batch_index`, `Source::batch_index`) let order-dependent sinks such as the
  batch result collector run in parallel and still keep the order. A sink with more
  parallel work after `finalize` schedules it in stages through `finalize_tasks` /
  `execute_finalize_task` (DuckDB's sink-specific finalize events).
- Operators must be correct when run on several threads with separate local states;
  merge in `combine`, finish in `finalize`. Results must not depend on the number of
  threads except where DuckDB's do not either (row order without `ORDER BY`).

## Testing

- **Unit tests** next to the code for invariants and edge cases.
- **Conformance**: `tools/slt` runs DuckDB's sqllogictest corpus against rustdb and the C++
  oracle. `conformance/passing.txt` is a ratchet: a file that passes must keep passing.
  Run `scripts/conformance.sh [--update] [paths]` after every merge of engine work.
- **Oracle tests** for value-level behaviour (casts, formatting, functions): generate
  inputs, ask the reference CLI, and compare byte for byte.
- **Benchmarks**: `tools/bench` runs DuckDB's benchmark suites on both engines;
  `bench/baseline.json` is the performance and memory ratchet.

## Working in parallel

- Each change is scoped to one subsystem and the test directories that exercise it.
- Agents work in their own git worktree branches and start from the commit they are given.
  They don't edit crates owned by another running task; additive changes to shared crates
  (new modules, new enum variants) are kept minimal and rebased before handing over.
