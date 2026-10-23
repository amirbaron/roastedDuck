# Handoff

State of the DuckDB 1.5.6 → Rust port ("rustdb") on the main line
(`amirbaron/rust-duckdb-port`), updated 2026-10-23, to continue the work in a new session.

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

- **Corpus**: the latest complete serial sweep has **4781 of 4870 pass**, 15 fail, 74 skip;
  C++ is 4795 pass, 1 fail, 74 skip. All remaining Rust failures are the 15 accepted
  divergences in `conformance/divergences.md` (11 C++ extension binaries, 4 expected
  memory errors Rust avoids). No unexplained regressions, crashes or timeouts.
  - The fresh callback/error-byte sweep plus the uncommitted cache flag uses normal thread
    defaults and passes the 4855-file ratchet without rechecks, shrink or verdict changes.
    Every skip reason matches the prior baseline; the 15 failure messages match too,
    apart from a process ID in a temporary filename. `target/slt/rust.jsonl` is refreshed
    from this full run, with its previous contents backed up beside the raw results.
  - An earlier 4870-file sweep ran with one worker and `SLT_THREADS=1`: 4779 pass,
    17 fail, 74 skip. Two tests require normal thread defaults (`test_batch_limit_filters`
    expects a parallel LIMIT, `reset_threads` compares startup threads with RESET); both
    pass on serial rechecks without the override. C++ reproduces the same one-thread
    behavior. That older baseline incorporated two rechecks; its raw results remain in
    the ICU artifact folder. The current canonical results need no reconciliation.
  - The ICU gap is closed (`2fa715c`): all 130 collations match C++ sort-key digests
    across 484,110 strings; all 24 collation files pass. The second gain over the
    previous 4779 baseline is a formerly skipped `require disk_space 40gb` TPC-H
    grouping test, freshly passed by both Rust and C++ as well.
  - `json_serialize_plan` is ported (1486 of 1524 plans byte-identical, work-queue 8).
    Database encryption is ported (`crates/crypto`); Parquet encryption remains open.
  The ratchet `conformance/passing.txt` lists **4855** files (4781 passes + 74 shared
  skips), with zero regressions and no shrink. The C++ reference's previously skipped
  grouping result and `conformance/dashboard.md` are refreshed.
- **Concurrency:** snapshot filtering and lost partial-write undo records are fixed
  (`506354f`). All 100 original stress runs, 20 diagnostic runs and the fresh full sweep
  pass. The earlier `max_swap_space_error.test` verification passed 20 of 20. Rerun a
  file alone and inspect its real failure before attributing it to a change.
- **C API:** 546 of 547 functions; all 30 `test/api/capi` files pass
  (`scripts/capi-tests/run.sh`, `--lib cpp` runs them against the C++ library). ADBC is
  missing. Fresh public binaries on `506354f` also pass 40 C API concurrent-write
  stress runs with the runner's zero-byte checkpoint threshold; 40 preserved-binary
  and ten C++ controls pass. The explicit-transaction lock sequence matches C++.
- **CLI shell (the owner's top priority; engine gaps remain):** `crates/shell` (`rustdb`) is DuckDB's
  shell one to one, linenoise included: all 392 `tools/shell/tests` that pass on the C++
  CLI pass (`scripts/shell-tests.sh --binary rust|cpp`), and 760 of 765 cases of
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
4781/15/74 results; the subsequent full sweep on `506354f` plus the cache flag confirms
them without rechecks. Shell verification passes with the same five known differences; all eight fresh
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

The working tree retains the uncommitted persistent-table cache experiment in
`crates/storage/src/table/column_segment.rs`. Modern plain/constraint ART disk restoration
and its publication fix are committed and pass the fresh full sweep; the remaining
paths stay open under work-queue 16. Candidate checks and both 160-query benchmark comparisons are saved under
`.context/artifacts/parity-20261022/cache-policy/`. All candidate shell/C API checks
and result comparisons pass, but the cache policy is **not accepted**: q21, q22 and
q28 have memory above C++, despite improved average speed. See the experimental table
in `docs/perf-backlog.md`. Preserve the uncached binaries in `uncached-merged/`.

The initial cache-only full sweep completed at **4780 pass, 16 fail, 74 skip**, zero crashes or
timeouts, with exactly one changed verdict against the prior baseline. The ratchet
correctly fails with one regression, `test/sql/pragma/profiling/test_profiling_fs.test`
(zero `TOTAL_BYTES_READ` on SELECT following reattach). A fresh C++ native-unittest
control passes, the preserved uncached Rust control passes, and the cache candidate
fails again in isolation. Source rebuilds plain indexes while loading the catalog,
warming table blocks at ATTACH. Native persisted-index restoration is work-queue item
16; `.context/index-loading-regression.md` records the investigation. Do not change
profiling counters or tests to hide this gap. The canonical ratchet/results were not
replaced by the failed candidate sweep.

The full-run path is recorded in `cache-policy/full-run-path.txt`; all worker logs and
`comparison.json` are retained. No workload is active after the serial allocation
diagnostic completed. Its report is `cache-policy/alloc-diagnosis/report.json`: cold
single-thread q21 and q28 retain similar table bytes to C++, with approximately 99 MB
and 350 MB extra peak-minus-retained-table memory respectively. This is an allocation
estimate, not a precise heap attribution. q22 used less memory than C++ in this single
CLI run despite its repeated C API benchmark outlier, so cache/filter-history effects
need controlled investigation. Fix the persisted-index regression first, then diagnose
or reject/rework caching's memory outliers before accepting the policy or claiming parity.
The previous signed binaries are the cache candidate; `uncached-merged/` preserves the
other verified variant. The native ART restoration build is complete and signed
(`.context/artifacts/index-reload-20261022/`). All 53 storage unit tests pass, including
modern/deprecated-leaf lazy reload and post-reload mutation. The profiling regression
passes again; 384 affected SQL files pass with nine shared skips (393 total). Ten
cross-engine file cases pass across storage 1.0/1.2/1.5 and GCM/CTR, including mutation,
reopen, cold ART residency and constraints. C++ encrypted fixture writes use its test
switch `force_mbedtls_unsafe`; normal C++ readers work without it. Workspace Clippy
with warnings denied, 30 C API files and 392 shell tests pass. The new full sweep
finished at 4779 pass, 17 fail and 74 skip, zero crashes/timeouts. Exactly two concurrent
checkpoint files regressed; all other verdicts match the canonical baseline. Its unique
path is in `index-reload-20261022/full-run-path.txt`, with `comparison.json` and worker logs.
The first full run of native reload exposed two concurrent checkpoint regressions:
`concurrent_checkpoint_insert.test` and `concurrent_checkpoint_wal_index.test_slow`
read newly serialized index locations before partial blocks were flushed. The
publication fix now requires an explicit writer flush before returning while the
bound-index guard or unbound-storage mutex is held, matching C++ lock/flush ordering.
All 55 storage unit tests pass, including deferred-write and failed-flush retry checks.
Both original concurrency files pass three targeted trials and again in the expanded
451-file cohort (441 passes, ten shared skips, no failures/crashes/timeouts). All ten
cross-engine file cases, workspace Clippy with warnings denied, 30 C API files and
392 shell tests pass on the fix. The old failing full results remain preserved.
The fresh full sweep on `cc5d2c2` plus the uncommitted cache flag finished at **4780
pass, 16 fail, 74 skip**, with zero crashes/timeouts. Both checkpoint regressions pass
in this sweep. The sole unexpected failure is now
`test/sql/update/test_stress_update_issue_19688.test`: the final concurrent
UPDATE/DELETE/INSERT check finds 999 distinct IDs rather than 1000. The ratchet fails
with one regression; canonical results and the 4855-file passing list remain unchanged.
The unique run path is in `index-reload-20261022/full-run-path.txt`; its `comparison.json`
and worker logs preserve the failure. Do not promote this failed run.

Serial repeated checks reproduce the candidate failure twice in 70 runs (68 passes).
The preserved uncached binary passes 69 of 70, with one earlier concurrent-iteration
failure whose abbreviated worker result does not establish the same cause. C++'s native
unittest passes all 20 controls. This is an unresolved concurrency issue, not a proven
cache or index-reload regression. Full logs for the additional 50 checks of each Rust
binary and JSON summaries are in `index-reload-20261022/stress-update/`. No workload is
active after those repeats. Diagnose the missing row before further parity acceptance.

The subsequent investigation reproduced two concurrency defects deterministically.
Column filtering re-read a growing vector's row count after the scan fixed its snapshot
limit; a new match in the final partial vector could be emitted with an old row ID.
Filtering now receives and respects the scan's maximum count. Separately, a conflict in
a later vector, column, validity mask or struct field discarded undo records for earlier
writes. Mutation records now propagate through errors and enter the transaction's undo
log before the error returns. This preserves C++'s behavior: an aborted explicit
transaction keeps earlier writes locked until `ROLLBACK`, then releases them. A live
C++ C API control confirms that lock sequence (`aborted-locks-cpp.log`).

All **80 storage tests pass** (55 unit, 14 MVCC, nine constraints, two type tests), and
workspace/all-targets Clippy with warnings denied passes. The six new regression tests
cover matching/nonmatching snapshot tails, conflicts across vectors and row groups,
multiple columns, NULL validity and struct fields. Each newly exposed defect failed
before its fix; the mutation tests also verify locks remain held before rollback.
The final MVCC rerun after the test-helper lint fix passes all 14 tests.

Release stress checks pass: **100 of 100 original runs and 20 of 20 diagnostic
runs**. The affected SQL cohort has **374 passes and one shared skip** (375 files),
zero failures/crashes/timeouts, and zero regressions against its scoped ratchet. The
first driver invocation incorrectly checked the subset against the entire ratchet and
stopped on missing unrelated files; the corrected scoped check passes without changing
the canonical list or rerunning the already completed stages.

The fresh full sweep is complete: **4781 pass, 15 accepted failures, 74 shared skips**,
zero crashes/timeouts, and **4855 listed, zero ratchet regressions**. The profiling,
both checkpoint and missing-ID stress regressions all pass. Every file's status and
skip reason matches the baseline; accepted failures match after normalizing one
temporary process ID. Raw results, worker logs, `comparison.json` and the prior canonical
result backup are in `full-20261022T211412Z-4173` under the index-reload artifact folder.
The canonical results are refreshed; dashboard counts and the passing list are unchanged.

The serial driver
`index-reload-20261022/stress-update/verify-journal.py` builds and signs the runner,
checks 100 original stress trials plus 20 diagnostic trials, checks the affected SQL
cohort and ratchet, then starts a fresh full sweep only if those gates pass. It stores
the source/binary manifest and all logs under `stress-update/verification/`. Poll the
live handle recorded in `.context/stress-update-investigation.md`; never launch a
duplicate workload. The previous failed full results and canonical ratchet remain
preserved. Shell/C API binaries still require rebuilding and
verification on the final source; an earlier partial release build was deliberately
stopped after the runner linked, before finishing the remaining packages.
The completed sweep ran through `resume-after-cohort.py`, which verified the
unchanged runner hash and source, checks the scoped ratchet, then resumes at the full
stage. Its unique directory is recorded in `index-reload-20261022/full-run-path.txt`.
`verify-post-full.py` completed successfully after that review. Fresh signed public
binaries pass all 30 C API files, 392 shell tests/three shared skips and the 727-case
shell comparison (zero unexpected differences, the same five known gaps). Forty C API
stress runs with the SLT checkpoint settings pass, as do 40 preserved-binary and ten
C++ controls. The live three-connection explicit-transaction lock sequence matches
C++ exactly. Its unique output directory is in `stress-update/post-full-path.txt`,
with `verdict.json` and signed identities. Both harnesses re-sign their public binary
before testing, so the manifest retains initial and final signed-file hashes. The SLT
hash remains the hash used for the full sweep. Both verification drivers are terminal;
no build or test workload remains active at this checkpoint.

The lazy type-catalog gap is now ported and verified. Builtin entries are real internal,
temporary, committed catalog objects: first lookup preserves spelling, scans create
the remaining lowercase defaults, and failed binds/rollbacks retain the same state as
C++. Suggestions compare the catalog map's original keys, including invisible versions.
Type binding follows C++'s fallback and qualification rules, keeps builtin modifiers,
and distinguishes type scopes from view scopes by default-schema precedence. This
fixes builtin shadowing and the cross-catalog ALTER enum fallback without weakening
the test. `duckdb_types()` reads actual entries and uses logical type IDs for internals.

Validation: six catalog unit tests, workspace/all-targets Clippy with warnings denied,
36 new unmarked byte-for-byte shell cases, the full 763-case shell comparison (zero
unexpected differences, five existing known gaps), and 882 affected SQL files (875
passes/seven shared skips, zero regressions). Two enum/schema/index files created by
opposite engines pass mutation/checkpoint/read-only reopen in both directions, with
unchanged hashes for read-only checks. The fresh full sweep has 4781 passes, 15 accepted
failures and 74 shared skips: every status, skip reason and normalized failure message
matches the baseline; the 4855-file ratchet is unchanged and passes.

Fresh public binaries pass all 30 C API files and 392 shell tests/three shared skips.
An 18-query, three-connection C API sequence matches C++ exactly, including enum vector
retrieval, connection-local defaults, rollback, shared types and schema precedence.
The comparison helper initially dereferenced a null legacy enum fetch; it now uses the
chunk/vector API on both engines. That helper fault did not require engine changes.
Artifacts: `.context/artifacts/shell-type-catalog-20261023/` (the final full,
verification and public directories have pointer files and manifests). The canonical
results are refreshed with a preserved prior copy; all drivers are terminal and no
build/test workload remains active. Remaining shell gaps and the cache performance
experiment stay open. `.context/shell-type-catalog-investigation.md` has the detailed
source findings and verification history.

Native disk restore covers plain and stored constraint indexes so far; expression/WAL,
pre-v0.10 and fallback paths remain open under work-queue 16. The cache policy is still
uncommitted and unaccepted because its memory/performance gates remain unresolved.
Details: `.context/index-reload-progress.md`.

The raw-byte shell work remains open. Its first shared boundary is implemented:
`Value::try_varchar(&[u8])` validates bytes with DuckDB's value-construction error
classification; the C API constructor and appender's fallback use it. Unchecked string
storage and the appender's direct paths retain their existing validation timing. All
102 type/vector unit tests and workspace/all-targets Clippy pass. Fresh signed public
binaries agree with C++ on 65,823 byte sequences (18,445 accepted, 47,378 rejected),
including exact accepted-byte round trips and embedded NULs; all 30 C API files and
392 shell tests/three shared skips pass. This behavior-preserving constructor relocation
does not close the raw input, parser, identifier or error-byte gaps. Artifacts and
manifests: `.context/artifacts/shell-bytes-20261023/`; source/probe notes:
`.context/shell-bytes-investigation.md`. No job is active after these checks.

The initial scalar error-byte boundary (`983e3eb`) preserves error-message bytes: `Error` owns bytes and
provides native byte formatters; scalar C API bind/init/execute callbacks and SQL
`error()` keep invalid UTF-8 intact. Native C API error accessors use those bytes.
NUL sanitization runs before JSON conversion, and JSON control escapes use DuckDB's
uppercase hex digits. `scripts/capi-error-bytes.py` compares 2,128 cases with C++:
all byte values and selected sequences, callback phases and raw-vector SQL errors,
direct/prepared statements, plain/JSON errors and connection recovery. All match.
The 23 common unit tests, workspace/all-targets Clippy, 30 C API files, all 765 shell
differential cases (zero unexpected differences, five known) and 392 upstream shell
tests/three shared skips pass on fresh signed binaries. Two new unmarked shell cases
cover NUL/control/Unicode messages and recovery. The full 4,870-file sweep has 4,781
passes, 15 accepted divergences, 74 shared skips and no crashes/timeouts; every status,
skip reason and normalized failure message matches the prior baseline. The 4,855-file
ratchet has zero regressions or new entries. Canonical results are refreshed with the
prior copy preserved. Public/full manifests and pointer files are under
`.context/artifacts/shell-bytes-20261023/` (`raw-errors-path.txt` and
`raw-errors-full-path.txt`). Both validation drivers are terminal. This does not close
raw shell input, parser/name bytes or legacy text error wrappers. The cache experiment
remains uncommitted and unaccepted.

The remaining native callback error carriers are now byte-based too: table,
aggregate (including deferred errors), COPY, replacement scans and casts. The shared
cast/JSON transform/scanner path preserves byte messages, source-column context and
DuckDB's first-nonempty TRY_CAST error. Aggregate state-size errors now occur during
physical preparation after optimization; SQL PREPARE handles its contained plan too.
Pruned aggregates do not invoke the callback. Window aggregate state-size errors stay
at execution, matching C++. JSON-mode prepare no longer appends a caret to raw messages
beginning with `{`.

The expanded `scripts/capi-error-bytes.py --family all` oracle has 13,832 unique cases:
2,128 scalar, 2,128 table, 5,320 aggregate, 2,128 COPY, 532 replacement and 1,596 cast.
It covers all byte values/selected sequences, plain/JSON, direct/prepared statements,
empty-input/deferred errors, combine, pruning, SQL PREPARE, window timing, CAST source
context, TRY_CAST NULL results and recovery. All match C++ byte for byte. All 35 signed
function-library unit tests and workspace/all-targets Clippy pass, as do all 30 C API
files, 765 shell cases (zero unexpected differences/five known), 392 upstream shell
tests/three skips and the full 4,870-file corpus (4,781 pass, 15 accepted divergences,
74 shared skips, zero crashes/timeouts). Every status, skip reason and normalized
failure message matches the previous baseline; the 4,855-file ratchet is unchanged.
Canonical results are refreshed with the prior copy preserved. Both drivers are
terminal; manifests/pointers are in `.context/artifacts/callback-bytes-20261023/`
(`first-path.txt`, `final-path.txt`). Raw shell/parser/name bytes and legacy text
wrappers remain open, as do the other work-queue and performance gates. The original
cache experiment is excluded from this callback change and remains unaccepted.

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
  workspace baseline was 668+ passes; this continuation executed 80 storage tests and
  the changed ICU crate (6/6), not the entire workspace unit suite.

## Next steps, in order

1. Continue the remaining shell engine differences, the owner's top priority
   (work-queue 3). Lazy type catalog/suggestion parity, its full corpus and fresh public
   checks are complete. Finish the other native
   persisted-index paths (16); the profiling and concurrency regressions are fixed.
   Resolve or reject/rework caching's memory
   outliers and continue the performance work; the policy is not accepted yet. Work
   down the rest of `docs/work-queue.md` sequentially. ICU verification is complete; Parquet encryption (15) remains.
2. Keep one workload at a time. Use `JOBS=1` for canonical conformance runs; do not
   force `SLT_THREADS=1` globally because tests of thread defaults and parallel plans
   intentionally depend on the normal setting. It remains useful for focused diagnostics.
3. TPC-DS is ported (`crates/tpcds`, all 25 files pass). Its `dsdgen`, like TPC-H's
   `dbgen` in `crates/tpch`, carries TPC's license, not MIT — the owner should review the
   TPC terms before rustdb is published; both stay optional extension crates so the core
   is MIT.
4. Performance rounds until the targets hold (≥ 1.0× C++ speed, ≤ 1.0× memory, allowing measured noise); then
   differential fuzzing against C++ (M9 in `docs/porting-plan.md`).
