# Handoff

State of the DuckDB 1.5.6 → Rust port ("rustdb") on the main line
(`amirbaron/rust-duckdb-port`), updated 2026-10-22, to continue the work in a new session.

## Goal

Every test of DuckDB's sqllogictest corpus passes, except the ones rustdb cannot pass by
design (`conformance/divergences.md`: C++ extension binaries, and tests that expect an
out-of-memory error rustdb avoids by using less memory). Along the way: byte-compatible
storage and WAL with C++ DuckDB, the same C API, no C/C++ in the shipped artifact, and the
performance targets of `docs/porting-plan.md` (≥ 1.0× C++ speed, ≤ 1.0× memory, allowing measured noise).

The owner confirmed on 2026-10-22: parity with C++ is sufficient; the former
≥ 1.3× speed / ≤ 0.6× memory targets are superseded. Functional and byte-compatibility
gates remain in place.

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

- **Corpus**: latest reconciled results are **4781 of 4870 pass**, 15 fail, 74 skip;
  C++ is 4795 pass, 1 fail, 74 skip. All remaining Rust failures are the 15 accepted
  divergences in `conformance/divergences.md` (11 C++ extension binaries, 4 expected
  memory errors Rust avoids). No unexplained regressions, crashes or timeouts.
  - A complete 4870-file sweep ran with one worker and `SLT_THREADS=1`: 4779 pass,
    17 fail, 74 skip. Two tests require normal thread defaults (`test_batch_limit_filters`
    expects a parallel LIMIT, `reset_threads` compares startup threads with RESET); both
    pass on serial rechecks without the override. C++ reproduces the same one-thread
    behavior. `target/slt/rust.jsonl` incorporates those two rechecks; raw full results
    remain in the ICU artifact folder.
  - The ICU gap is closed (`2fa715c`): all 130 collations match C++ sort-key digests
    across 484,110 strings; all 24 collation files pass. The second gain over the
    previous 4779 baseline is a formerly skipped `require disk_space 40gb` TPC-H
    grouping test, freshly passed by both Rust and C++ as well.
  - `json_serialize_plan` is ported (1486 of 1524 plans byte-identical, work-queue 8).
    Database encryption is ported (`crates/crypto`); Parquet encryption remains open.
  The ratchet `conformance/passing.txt` lists **4855** files (4781 passes + 74 shared
  skips), with zero regressions and no shrink. The C++ reference's previously skipped
  grouping result and `conformance/dashboard.md` are refreshed.
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
`scripts/icu-collation-data.sh` and `scripts/conformance.sh` default to `JOBS=1`
for subsequent exporter builds and corpus runs.
The port includes a deliberate termination guard: on rare inputs where ICU's FCD iterator
loops forever, Rust collates the NFD form instead. Such reference hangs are excluded from
byte comparisons; a Rust unit test covers the fallback.

## How to work

- Build and sign: `cargo build --release -p slt -j 1 && scripts/sign.sh target/release/slt`.
- One file: `target/release/slt exec --engine rust --root .context/duckdb-download/duckdb-1.5.6 test/...`.
- Subset: `JOBS=1 scripts/conformance.sh test/sql/<dir> ...`; full:
  `JOBS=1 scripts/conformance.sh > "/Volumes/amirs ssd/ai context/conductor/duckdb-rust/logs/main/<name>.log" 2>&1`.
- The C++ reference CLI is `.context/duckdb-download/duckdb-1.5.6/build/release/duckdb`
  (run it from /tmp); its library is `.../build/release/src/libduckdb.dylib`. Use it to
  settle exact behavior and error texts and for cross-engine file round trips.
- Merging an agent branch: conflicts in `conformance/dashboard.md` take main's side (it is
  regenerated); check `conformance/passing.txt` stays sorted without duplicates; run
  `cargo check --workspace --all-targets -j 1`, the affected corpus directories, then a full run.
- Unit tests: build with `cargo test --workspace --release -j 1 --no-run`, sign the
  generated test executables, then run them serially with `--test-threads=1`. The older
  workspace baseline was 668+ passes; this continuation executed the changed ICU crate
  (6/6), not the entire workspace unit suite.

## Next steps, in order

1. Review the saved performance round 6 work, then work down `docs/work-queue.md`
   sequentially. ICU verification is complete; Parquet encryption (15) remains.
2. Keep one workload at a time. Use `JOBS=1` for canonical conformance runs; do not
   force `SLT_THREADS=1` globally because tests of thread defaults and parallel plans
   intentionally depend on the normal setting. It remains useful for focused diagnostics.
3. TPC-DS is ported (`crates/tpcds`, all 25 files pass). Its `dsdgen`, like TPC-H's
   `dbgen` in `crates/tpch`, carries TPC's license, not MIT — the owner should review the
   TPC terms before rustdb is published; both stay optional extension crates so the core
   is MIT.
4. Performance rounds until the targets hold (≥ 1.0× C++ speed, ≤ 1.0× memory, allowing measured noise); then
   differential fuzzing against C++ (M9 in `docs/porting-plan.md`).
