# Handoff

State of the DuckDB 1.5.6 → Rust port ("rustdb") on the main line
(`amirbaron/rust-duckdb-port`), updated 2026-10-22, to continue the work in a new session.

## Goal

Every test of DuckDB's sqllogictest corpus passes, except the ones rustdb cannot pass by
design (`conformance/divergences.md`: C++ extension binaries, and tests that expect an
out-of-memory error rustdb avoids by using less memory). Along the way: byte-compatible
storage and WAL with C++ DuckDB, the same C API, no C/C++ in the shipped artifact, and the
performance targets of `docs/porting-plan.md` (≥ 1.3× C++ speed, ≤ 0.6× memory).

## Ground rules (from the owner)

- **At most two agents at a time, machine-wide, and no nested agents.** More runs the Mac
  out of memory and hits usage limits. Brief agents to not spawn sub-agents.
- **Only one full corpus run at a time** on the machine
  (`ps -eo pid,etime,command | grep "slt run"`), and never rebuild `target/release/slt` in
  the same target directory while a full run uses it (the harness launches it per file).
- **Every built binary must be signed** with `scripts/sign.sh <binary>` (the rustdb-dev
  identity) before it runs, or it cannot access the external drive: `target/release/slt`,
  `target/release/rustdb`, `target/release/libduckdb.dylib`, test executables. Re-sign
  after every build.
- Big files on the external SSD: `/Volumes/amirs ssd/ai context/conductor/duckdb-rust/`
  (`targets/<name>`, `bench-data`, `logs/<name>`, `tmp`); `target` in this workspace is a
  symlink to `targets/ottawa`. Agents use `CARGO_TARGET_DIR=.../targets/<name>` plus
  `ln -sfn "$CARGO_TARGET_DIR" target` in their worktree.
- Subagents use model "opus". Commit messages end with the Co-Authored-By line; docs
  changes go in their own commit before the code.

## Where things stand

- **Corpus** (`scripts/conformance.sh`, last full run on main): 4752 of 4870 files pass,
  18 fail, 100 skip, no timeouts. C++ DuckDB
  passes 4794. Of the files C++ passes and rustdb does not:
  - 25 skip with `require tpcds` (the `tpcds` extension is not ported; its `dsdgen` is
    TPC-licensed code, see below);
  - 15 are accepted divergences (`conformance/divergences.md`: extension binaries, and
    out-of-memory errors rustdb avoids);
  - the rest are real gaps, each in `docs/work-queue.md`: `buffer_managed_query_result`,
    `physical_operator_logging` and the external file cache (2) — agents are on these —, `json_serialize_plan`,
    encryption
    (`dict_fsst_expression_filter`), ICU `as`/`kok`/`mr`.
  The ratchet `conformance/passing.txt` lists 4827 files and must never shrink:
  `target/release/slt ratchet update target/slt/rust.jsonl` after a run.
- **Flaky:** none known since the buffer-memory merge (`max_swap_space_error.test` passed
  20 of 20); rerun a file alone before treating a failure under load as a regression.
- **C API:** 546 of 547 functions; all 30 `test/api/capi` files pass
  (`scripts/capi-tests/run.sh`, `--lib cpp` runs them against the C++ library). ADBC is
  missing.
- **CLI shell (the owner's top priority, done):** `crates/shell` (`rustdb`) is DuckDB's
  shell one to one, linenoise included: all 392 `tools/shell/tests` that pass on the C++
  CLI pass (`scripts/shell-tests.sh --binary rust|cpp`), and 717 of the 726 cases of
  `scripts/shell-diff.py` (modes, types, dot commands, options, errors, pty sessions)
  match byte for byte (721 of 727; the 6 others are known engine differences,
  work-queue item 3).
  `duckdb_functions()` matches DuckDB's except TPC-DS, `json_serialize_plan` and
  `duckdb_external_file_cache`.
- **Compression:** `crates/zstd` ports zstd 1.5.6's encoder and decoder (zstd's frames at
  levels -5..3; ruzstd is gone); roaring, hugeint bitpacking and partial blocks match
  DuckDB's layouts (work-queue item 5 lists what is left).
- **Performance** (`docs/perf-backlog.md`, round 5; speed = C++ time ÷ rustdb time, higher
  is better; memory = rustdb ÷ C++, lower is better):

  | suite | speed 14 / 1 threads | memory 14 / 1 threads |
  |---|---|---|
  | TPC-H SF10 | 0.84× / 0.92× | 0.28× / 0.20× |
  | ClickBench | 0.88× / 0.90× | 0.35× / 0.28× |
  | h2o group-by | 1.01× / 1.07× | 0.73× / 0.36× |
  | h2o joins | 1.10× / 1.22× | 0.78× / 0.63× |

  Caching table blocks like DuckDB was measured and dropped (memory rises to DuckDB's).

## Agents at work (2026-10-22)

Worktrees under `/Users/amirbaron/src/duckdb-rust/.claude/worktrees/`, briefs in
`.context/briefs/`:

- TPC-DS (`tpcds.md`, base `1328ebc`);
- logical plan serialization (`plan-serialization.md`, base `9aa5053`).

Queued, in order (briefs written, each with `<BASE>` to fill in): `encryption.md`, `icu66.md` (ICU 66
collation data and sort key bytes); `perf-round-6.md` (performance rounds, repeat until
the targets hold). `_common.md` holds the
ground rules every brief repeats. If a session ends while
an agent runs, its worktree holds its commits: review, finish or salvage, merge, run the
full corpus.

## How to work

- Build and sign: `cargo build --release -p slt && scripts/sign.sh target/release/slt`.
- One file: `target/release/slt exec --engine rust --root .context/duckdb-download/duckdb-1.5.6 test/...`.
- Subset: `scripts/conformance.sh test/sql/<dir> ...`; full:
  `scripts/conformance.sh > "/Volumes/amirs ssd/ai context/conductor/duckdb-rust/logs/main/<name>.log" 2>&1`.
- The C++ reference CLI is `.context/duckdb-download/duckdb-1.5.6/build/release/duckdb`
  (run it from /tmp); its library is `.../build/release/src/libduckdb.dylib`. Use it to
  settle exact behavior and error texts and for cross-engine file round trips.
- Merging an agent branch: conflicts in `conformance/dashboard.md` take main's side (it is
  regenerated); check `conformance/passing.txt` stays sorted without duplicates; run
  `cargo check --workspace --all-targets`, the affected corpus directories, then a full run.
- Unit tests: `cargo test --workspace --release --no-fail-fast` (668+ pass; the parser's
  deep-expression test and a racy scheduler test were fixed on 2026-10-21).

## Next steps, in order

1. Merge the running agents' branches (TPC-DS, plan serialization), then run the queued
   briefs.
2. Close the remaining gaps in `docs/work-queue.md`, at most two agents at a time:
   encryption, ICU
   collation for as/kok/mr and its sort key bytes.
3. TPC-DS (25 tests): port the `tpcds` extension. Its `dsdgen` (like TPC-H's `dbgen`,
   already in `crates/tpch`) carries TPC's license, not MIT — the owner should review
   the TPC terms before rustdb is published; keeping both generators as optional
   extensions keeps the core MIT.
4. Performance rounds until the targets hold (≥ 1.3× C++ speed, ≤ 0.6× memory); then
   differential fuzzing against C++ (M9 in `docs/porting-plan.md`).
