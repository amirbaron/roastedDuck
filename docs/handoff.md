# Handoff

State of the DuckDB 1.5.6 → Rust port ("rustdb") at main `37d66fe`, written 2026-10-21 to
continue the work in a new session.

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

- **Corpus** (`scripts/conformance.sh`, last full run on main): 4726 of 4870 files pass,
  42 fail, 101 skip, 1 timeout (`variant/tpch_sf1`, which C++ cannot finish in 300 s
  either). C++ DuckDB passes 4794; 4725 pass on both. Of the 69 files C++ passes and
  rustdb does not:
  - 25 skip with `require tpcds` (the `tpcds` extension is not ported; its `dsdgen` is
    TPC-licensed code, see below), 1 with `require core_functions` (likely a naming fix);
  - about 14 are accepted divergences (`conformance/divergences.md`);
  - about 30 are real gaps, each in `docs/work-queue.md`.
  The ratchet `conformance/passing.txt` lists 4802 files and must never shrink:
  `target/release/slt ratchet update target/slt/rust.jsonl` after a run.
- **Flaky:** `test/sql/storage/temp_directory/max_swap_space_error.test` (4 in 10 runs;
  work-queue item 14). Rerun it alone before treating it as a regression.
- **C API:** 546 of 547 functions; all 30 `test/api/capi` files pass
  (`scripts/capi-tests/run.sh`, `--lib cpp` runs them against the C++ library). ADBC is
  missing.
- **CLI shell:** `crates/shell` is still a stub (prints `a|b` rows).
- **Performance** (`docs/perf-backlog.md`, round 4; speed = C++ time ÷ rustdb time, higher
  is better; memory = rustdb ÷ C++, lower is better):

  | suite | speed 14 / 1 threads | memory |
  |---|---|---|
  | TPC-H SF10 | 0.86× / 0.93× | 0.28× |
  | ClickBench | 0.85× / 0.85× | 0.35× |
  | h2o group-by | 1.00× / 0.98× | 0.73× |
  | h2o joins | 1.12× / 1.20× | 0.79× |

## Unmerged work (resume or salvage first)

Both agents stopped at a usage limit; their work is in worktrees under
`/Users/amirbaron/src/duckdb-rust/.claude/worktrees/`:

- `agent-a567619c4a383073a` (branch `worktree-agent-a567619c4a383073a`, performance
  round 5, base `4f57ddc`): six commits — UnnestRewriter pattern 2, PhysicalPartitionedAggregate,
  checkpoint data blocks from the free list, **unpinned persistent blocks kept in the
  eviction queue** (the block-caching change), buffer-managed delim join input, finalize
  table sizing — plus 26 uncommitted files (it was editing Top-N and `DynamicFilterData`).
  Not benchmarked or corpus-tested yet.
- `agent-a88c4fa503c8056f3` (branch `worktree-agent-a88c4fa503c8056f3`, CLI shell port,
  base `96d68d7`): no commits, 17 uncommitted files (a duckbox renderer and the start of the
  shell's execution module).

To continue one in a new session, start an agent in that worktree with a brief that says
what it was doing, or review and commit the work there yourself; then build, sign, run the
affected corpus directories, merge into main, and run the full corpus.

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
- Unit tests: `cargo test --workspace --release` (all green at `37d66fe`).

## Next steps, in order

1. Salvage and finish the shell port — the owner's top priority: DuckDB's shell one to
   one — and performance round 5 (above); merge each.
2. Close the real gaps in `docs/work-queue.md`, at most two agents at a time:
   compression parity (zstd encoder, roaring/bitpacking layouts), the in-memory result
   collection and buffer eviction under tight limits, plan serialization, operator
   logging, external file cache, `ALTER DATABASE` rename, sequence ownership, ICU
   collation for as/kok/mr, VARIANT extract pushdown.
3. TPC-DS (25 tests): port the `tpcds` extension. Its `dsdgen` (like TPC-H's `dbgen`,
   already in `crates/tpch`) carries TPC's license, not MIT — the owner should review
   the TPC terms before rustdb is published; keeping both generators as optional
   extensions keeps the core MIT.
4. Performance rounds until the targets hold; then differential fuzzing against C++
   (M9 in `docs/porting-plan.md`).
