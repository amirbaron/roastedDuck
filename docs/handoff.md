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
- **CLI shell (the owner's top priority; engine gaps remain):** `crates/shell` (`rustdb`) is DuckDB's
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
- **Performance** (`docs/perf-backlog.md`, fresh round 6 baseline; speed = C++ time ÷
  Rust time, memory = Rust ÷ C++; 160 result checks pass):

  | suite | speed 14 / 1 threads | memory 14 / 1 threads |
  |---|---|---|
  | TPC-H SF10 | 0.88× / 0.94× | 0.29× / 0.21× |
  | ClickBench | 0.87× / 0.91× | 0.36× / 0.29× |
  | h2o group-by | 1.03× / 0.99× | 0.72× / 0.40× |
  | h2o joins | 1.16× / 1.26× | 0.79× / 0.50× |

  H2O joins meet the revised gates on this run; the other suites have individual slow
  queries. ClickBench database-open q00 uses about 3.2 MB extra peak RSS (1.11× above
  the interpreter). Performance parity is open. C++-style table-block caching, formerly
  dropped for the stricter memory target, is the next experiment.

## Saved work (2026-10-22)

No previous Rust build, corpus run or agent process was active when this session resumed.
The ICU worktree (`worktree-agent-a57faf12816ee7338`) has been merged; keep it and its
artifacts intact. Performance round 6's five saved commits (`1814548` through `155fe70`) are
integrated as `2a4541a` through `1e53786`. They pool vector buffers by size, speed
RLE skips, keep all-row selections implicit, reuse compressed string heaps, and reuse
expression scratch lists and constants. A retained-result lifetime regression test was
added (`547a13b`). All 202 focused release unit tests pass; workspace/all-targets check
and Clippy with warnings denied pass. The focused SQL run has 200 passes and three
shared skips (203 files), with no failures, crashes or timeouts and zero regressions
against the 4855-file ratchet. These focused rechecks retain the prior overall
4781/15/74 results; a new full sweep is still required. Shell verification passes with the same five known differences; all eight fresh
performance suites are complete with 160 passing result checks. Performance acceptance
remains open because the parity targets are not yet met.

The benchmark harness now fails on mismatches or engine errors and retains its
check verdicts in JSON (`65042a1`); six regression tests pass. Older JSON lacks those
verdicts and can conceal failures: saved `r6base-hits-t1.txt` includes a C++ q10 open
failure, so that aggregate is incomplete. New evidence lives under
`.context/artifacts/parity-20261022/`, including the pre-change binaries, unit logs,
focused SQL results and ratchet check. Global rustfmt also finds pre-existing formatting
in untouched files; the integrated code was formatted without unrelated churn. Its brief is `.context/briefs/perf-round-6.md` (base `4dca105`).
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

## Active cache experiment (2026-10-22)

The current working tree has one uncommitted code change in
`crates/storage/src/table/column_segment.rs`: retain persistent table blocks as C++
does. Candidate checks and both 160-query benchmark comparisons are saved under
`.context/artifacts/parity-20261022/cache-policy/`. All candidate shell/C API checks
and result comparisons pass, but the cache policy is **not accepted**: q21, q22 and
q28 have memory above C++, despite improved average speed. See the experimental table
in `docs/perf-backlog.md`. Preserve the uncached binaries in `uncached-merged/`.

A fresh 4870-file sweep is running with one worker, normal SQL thread defaults, and
worker logs preserved on the SSD. Its path is recorded in `cache-policy/full-run-path.txt`.
Do not start another workload or rebuild `target/release/slt` until it finishes. After
it finishes, inspect every result against the prior 4781/15/74 baseline and the ratchet;
then diagnose or reject the cache policy's memory outliers before claiming parity.
The running candidate sweep has exposed one new regression so far:
`test/sql/pragma/profiling/test_profiling_fs.test` (zero `TOTAL_BYTES_READ` on SELECT
following reattach). Source points to rebuilding plain indexes while loading the
catalog, warming table blocks at ATTACH. Native persisted-index restoration is now
work-queue item 16; `.context/index-loading-regression.md` records the investigation.
Do not change profiling counters or tests to hide this gap.

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

1. Finish verification and measurements of the integrated performance round 6 work,
   then work down `docs/work-queue.md` sequentially. ICU verification is complete; Parquet encryption (15) remains.
2. Keep one workload at a time. Use `JOBS=1` for canonical conformance runs; do not
   force `SLT_THREADS=1` globally because tests of thread defaults and parallel plans
   intentionally depend on the normal setting. It remains useful for focused diagnostics.
3. TPC-DS is ported (`crates/tpcds`, all 25 files pass). Its `dsdgen`, like TPC-H's
   `dbgen` in `crates/tpch`, carries TPC's license, not MIT — the owner should review the
   TPC terms before rustdb is published; both stay optional extension crates so the core
   is MIT.
4. Performance rounds until the targets hold (≥ 1.0× C++ speed, ≤ 1.0× memory, allowing measured noise); then
   differential fuzzing against C++ (M9 in `docs/porting-plan.md`).
