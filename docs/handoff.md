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

- **One task and one build or test workload at a time** (owner, 2026-10-22). Do not
  launch subagents or overlap builds, conformance runs and benchmarks. Use `-j 1` for
  Cargo, `--jobs 1` for test runners, and low process priority (`nice -n 10`).
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

- **Corpus** (`scripts/conformance.sh`, last full run on main): 4779 of 4870 files pass,
  16 fail, 75 skip (both engines skip the same files: extensions and settings the
  reference build lacks), no timeouts. C++ DuckDB passes 4794. Of the 16 files C++ passes
  and rustdb does not:
  - 15 are accepted divergences (`conformance/divergences.md`: 11 load C++ extension
    binaries, 4 expect out-of-memory errors rustdb avoids by using less memory);
  - The ICU `as`/`kok`/`mr` gap is merged as `2fa715c`. All 30 affected collation,
    timezone and plan-serialization files pass on the merged code; the next full run
    must confirm the new total. Fresh C++ comparisons match for all 130 collations
    across 484,110 input strings, plus locale keywords, names and BCP 47 tags.
    `json_serialize_plan` is ported (1486 of 1524 plans byte-identical, work-queue 8).
    Database encryption is ported (`crates/crypto`): `dict_fsst_expression_filter`
    passes, and so do the encryption tests that `require httpfs` without that line.
  The ratchet `conformance/passing.txt` lists 4855 files and must never shrink:
  `target/release/slt ratchet update target/slt/rust.jsonl` after a run.
- **Flaky:** none known since the buffer-memory merge (`max_swap_space_error.test` passed
  20 of 20); rerun a file alone before treating a failure under load as a regression.
- **C API:** 546 of 547 functions; all 30 `test/api/capi` files pass
  (`scripts/capi-tests/run.sh`, `--lib cpp` runs them against the C++ library). ADBC is
  missing.
- **CLI shell (the owner's top priority, done):** `crates/shell` (`rustdb`) is DuckDB's
  shell one to one, linenoise included: all 392 `tools/shell/tests` that pass on the C++
  CLI pass (`scripts/shell-tests.sh --binary rust|cpp`), and 722 of 727 cases of
  `scripts/shell-diff.py` (modes, types, dot commands, options, errors, pty sessions)
  match byte for byte; the five others are known engine differences (work-queue item 3).
  Both suites were reverified after the ICU merge with one worker. Upstream shell tests
  leave temporary paths unquoted; `pytest-ssd` in the ICU artifact folder exposes
  `tmp_path` through the workspace symlink so all test files remain on the SSD.
  `duckdb_functions()` matches DuckDB's exactly.
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

## Saved work (2026-10-22)

No previous Rust build, corpus run or agent process was active when this session resumed.
The ICU worktree (`worktree-agent-a57faf12816ee7338`) has been merged; keep it and its
artifacts intact. Performance round 6 remains saved work to review after ICU verification,
not a running agent. Its brief is `.context/briefs/perf-round-6.md` (base `4dca105`).
Do not launch the historical briefs in parallel: the owner's latest limit is one task.

ICU verification artifacts are under
`.context/artifacts/icu66-resume-20261022/` (the workspace's dedicated external SSD
folder). `differential.py` reruns both engines serially against the saved Unicode inputs.
The six ICU unit tests, workspace/all-targets check and Clippy pass without warnings.
Re-exporting with the saved offline tool reproduces all 2,680,616 data bytes (SHA-256
`9b5516e7609c98b7e25b6ed27ae60ae2ae49ebe1386fa8ef3bccb37b9df3d5ac`).
`scripts/icu-collation-data.sh` defaults to `JOBS=1` for subsequent exporter builds.
The port includes a deliberate termination guard: on rare inputs where ICU's FCD iterator
loops forever, Rust collates the NFD form instead. Such reference hangs are excluded from
byte comparisons; a Rust unit test covers the fallback.

## How to work

- Build and sign: `cargo build --release -p slt -j 1 && scripts/sign.sh target/release/slt`.
- One file: `target/release/slt exec --engine rust --root .context/duckdb-download/duckdb-1.5.6 test/...`.
- Subset: `JOBS=1 SLT_THREADS=1 scripts/conformance.sh test/sql/<dir> ...`; full:
  `JOBS=1 SLT_THREADS=1 scripts/conformance.sh > "/Volumes/amirs ssd/ai context/conductor/duckdb-rust/logs/main/<name>.log" 2>&1`.
- The C++ reference CLI is `.context/duckdb-download/duckdb-1.5.6/build/release/duckdb`
  (run it from /tmp); its library is `.../build/release/src/libduckdb.dylib`. Use it to
  settle exact behavior and error texts and for cross-engine file round trips.
- Merging an agent branch: conflicts in `conformance/dashboard.md` take main's side (it is
  regenerated); check `conformance/passing.txt` stays sorted without duplicates; run
  `cargo check --workspace --all-targets`, the affected corpus directories, then a full run.
- Unit tests: `cargo test --workspace --release --no-fail-fast` (668+ pass; the parser's
  deep-expression test and a racy scheduler test were fixed on 2026-10-21).

## Next steps, in order

1. Finish merged ICU verification, including the full corpus with one worker.
2. Review the saved performance round 6 work, then work down `docs/work-queue.md`
   sequentially. ICU orders and sort keys are ported; Parquet encryption (15) remains.
3. TPC-DS is ported (`crates/tpcds`, all 25 files pass). Its `dsdgen`, like TPC-H's
   `dbgen` in `crates/tpch`, carries TPC's license, not MIT — the owner should review the
   TPC terms before rustdb is published; both stay optional extension crates so the core
   is MIT.
4. Performance rounds until the targets hold (≥ 1.3× C++ speed, ≤ 0.6× memory); then
   differential fuzzing against C++ (M9 in `docs/porting-plan.md`).
