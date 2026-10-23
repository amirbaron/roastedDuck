# Handoff

State of the DuckDB 1.5.6 → Rust port ("rustdb") on the main line
(`amirbaron/rust-duckdb-port`), updated 2026-10-23, to continue the work in a new session.

## Goal

Every test of DuckDB's sqllogictest corpus passes, except the ones rustdb cannot pass by
design (`conformance/divergences.md`: C++ extension binaries, and tests that expect an
out-of-memory error rustdb avoids by using less memory). Along the way: byte-compatible
storage and WAL with C++ DuckDB, the same C API, no C/C++ in the shipped artifact, and the
documented speed–memory acceptance of `docs/performance-acceptance.md`.

The owner updated the goal on 2026-10-23: preserve the measured lower-memory baseline;
roughly 15–20% longer execution with 60–70% less memory on TPC-H and ClickBench is
acceptable. Exact speed/memory parity and outperformance are not required. Do not raise
memory merely to copy C++ caching or close small speed gaps; report individual query
outliers. Functional, correctness and storage/WAL compatibility gates remain in place.
Complete Python bindings first and all primary clients plus C#/.NET, Swift and Julia;
`docs/language-bindings.md` records the expanded completion scope.

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

## Current source integration (2026-10-23)

The checkout has an **uncommitted native-identifier migration and does not build as a
complete workspace yet**. The planner library now compiles after native renderer and
SHOW literal integration. The built-in function library also compiles after native
STRUCT/UNION/ENUM and result-name integration. The latest workspace/all-targets check
now passes execution, CSV and autocomplete compilation; the latest workspace check stops at **53 Parquet diagnostics**. Runtime and client/result
consumers remain unchecked by a successful complete build.
The corpus, shell and performance results below describe previously accepted binaries;
they are not validation of this incomplete source. No final public binary or full
corpus/file/WAL run has been made for the migration.

Names now retain bytes through AST/type/value codecs, parser transformations, casts,
storage filters/statistics/index descriptors and parts of planner name resolution.
The remaining work includes native expression/value rendering integration,
downstream result/execution integration and raw SQL parser ingress. Catalog SDK raw-name
file round trips pass, but public SQL/shell and WAL replay remain unverified. Client
packages and final performance acceptance also remain open; keep the uncached lower-memory controls unchanged.

New focused evidence includes 5,241 C++ index descriptor cases, 6,988 CTE/macro/lambda
codec cases, 3,494 native exception metadata cases, and 8,050 parser AST/rendering
comparisons plus 26 acceptance/rejection decisions. The 700 invalid-UTF8 parser inputs
remain pending raw ingress and are not counted as passed. Lambda-reference codec/name
checks do not establish SQL rendering parity: C++ deliberately rejects rendering that
transient node, while the current Rust renderer returns its name. Record and close
that difference before final acceptance.

The latest table/pivot stage passes all 218 affected release tests (one pre-existing
exhaustive cast-grid test remains ignored), 15,723 new C++ pivot/value comparisons and
the previous 8,735 cast comparisons. The 15,723 cases cover 3,494 pivot/unpivot AST
wire/render cases and 12,229 value/cast cases; the latter compare successes and exact
exceptions, including 841 plain/cast and 561 SQL exception outcomes. C++ validates
VARCHAR when materializing values: nested values with invalid name bytes can fail
`ToString`, even though a STRUCT SQL literal can keep those bytes.

`Value::to_varchar_bytes` and `try_to_sql_string` expose that native behavior. The
infallible `to_sql_string` wrapper and old text callers still need migration so rendering
exceptions reach the engine/shell; this is not complete native rendering parity. An
unnamed-STRUCT-to-VARIANT probe also fixed a real cast-error distinction: C++ exceptions
from cast binding/execution escape unchanged; only recorded conversion failures get
"Failed to cast value". Scoped common/types/function/AST/parser Clippy with warnings
denied and codegen --check pass. Final workspace compilation and runtime gates remain
open. Exact logs and hashes: `table-name-checkpoint.json` in the external artifact folder.

The statement-name stage now carries INSERT target qualifiers/columns, UPDATE SET
columns, MERGE action/source names, JOIN USING names and COPY column metadata as bytes.
It passes all 86 affected function/AST/parser release tests (one pre-existing exhaustive
cast-grid test remains ignored), 5,241 new C++ comparisons and the prior parser oracle.
The new cases comprise 1,747 INSERT SQL renderings, 1,747 JOIN USING wire/restored-SQL
cases and 1,747 COPY ParseInfo wire/name cases. COPY cases do not verify COPY SQL text or
file execution. Scoped Clippy with warnings denied and codegen --check pass.

The workspace still does not build. Remaining statement and table-name errors reach
real catalog/search-path APIs and filesystem boundaries that must accept native bytes;
do not decode names to text merely to make compilation succeed. No full engine or
client/runtime acceptance is established by these focused tests. Evidence and source
hashes are in `statement-name-checkpoint.json` under the external artifact folder.

The native catalog/storage stage now passes **287 core release tests plus 23 parser
release tests** (one pre-existing exhaustive cast-grid test remains ignored), scoped
all-target Clippy and codegen checks. Catalog names/keys, object/dependency/view names,
type aliases, table storage names and catalog undo/WAL/checkpoint carriers retain bytes.
Snapshot rollback/rename tests cover all 256 byte values, and Unicode names are kept
distinct under C++'s ASCII-only identifier folding.

The four preserved C++ raw-name databases (table, column, schema and enum-type names)
and their four UTF-8 controls now load and scan through the signed Rust catalog/storage
SDK. Copies were appended to, checkpointed by Rust, reopened by Rust and queried by the
C++ CLI: **all eight cross-engine round trips pass**, with two expected rows each. All
original hashes are unchanged. A native C++ metadata probe confirms that the stored enum
column has no alias; an initial harness assumption expecting one was corrected. The
reader is a Rust catalog example, not the public Rust shell: full Rust SQL/shell and WAL
recovery remain unverified. Round-trip report pointer:
`identifier-bytes-20261023/catalog-file-roundtrip-path.txt` under the external artifact
root. No C++ DuckDB engine is linked into the Rust reader.

Context search paths/lookups, CREATE/ALTER and table metadata now retain bytes through
their planner interfaces. Transaction database names and sequence WAL records use byte
carriers and explicit native string serialization. Prepared-parameter maps, export
metadata, COLUMNS aliases and debug plan rendering have also been integrated. Export
filename sanitization now replaces each non-letter byte, matching the frozen C++ source.
That checkpoint had seven planner diagnostics; the renderer continuation below closes
those library compilation errors. Downstream execution/result consumers have not yet
been checked by a successful full build. Rebuild the entire engine and repeat real SQL, shell, corpus
and WAL journeys before claiming acceptance. The logger interface now has a native byte-message method; the runtime logger
implementation and native plan/value formatting callers still require integration.
These remaining gates, language clients and final performance acceptance stay open.

The Context/binder continuation passes **310 core/parser release tests**, with the same
one pre-existing exhaustive cast-grid test ignored. All affected core packages pass
all-target Clippy with warnings denied; codegen --check passes. These tests do not build
or execute the planner. Logs: `context-core-stage-verification.json`,
`context-core-clippy.log` and `context-codegen-check.log` in the external artifact folder.

The renderer continuation now matches **5,832 C++ outcomes**, comparing exact output
or raw exceptions: 4,608 single-byte cases and 1,224 Unicode/layout cases across six
formats, three name/value fields and trees with children. **32 common release tests**
pass, including a 372-case frozen C++ golden subset; scoped all-target Clippy with
warnings denied passes. This verifies the native tree component, not complete public
EXPLAIN/expression/value integration or shell behavior.

RenderTree/RenderParams/output now own bytes and rendering returns Result. Text layout
uses 13,032 bytes of frozen utf8proc Unicode 15.1 properties and Rust grapheme rules;
no C++ is linked. The port matches C++'s invalid-value omission, JSON validation/NUL
handling and uppercase control escapes, empty-line splitting, ASCII-CI first-value map
semantics and target-C-char Mermaid trimming/splitting behavior. Reproducible offline
export/oracle sources are in scripts/; binaries, full outcomes and earlier mismatch
logs remain on the external SSD. Original raw-name databases remain unchanged.

A read-only C++ journey confirms raw-schema SELECT succeeds while SHOW TABLES FROM
that same schema fails with invalid unicode in value construction; the UTF-8 control
succeeds. SHOW now validates its generated VARCHAR filter literal at that actual value
boundary, while catalog/search-path identities retain bytes. Rust public SQL/shell
rechecks remain pending the full build. Renderer evidence: renderer-checkpoint.json under the same external artifact folder;
the following function stage advances the build further. All language clients and final performance acceptance remain
open; keep the uncached lower-memory controls unchanged.

The built-in-function/ENUM continuation passes **197 affected release tests**, with the
same one pre-existing exhaustive cast grid ignored. Types/function/built-ins and
storage/catalog/planner all-target Clippy pass with warnings denied; codegen checks pass.
ENUM dictionaries and codecs now retain bytes through casts, UNION tags and native SQL
rendering. **8,735 C++ ENUM comparisons** match exact wire data, rendered bytes or errors,
including 560 expected exception outcomes. The 50-case golden subset is checked in.

Two new C++ databases (raw ENUM label and UTF-8 control) load through the signed Rust
catalog/storage SDK. Rust appends the second ENUM code on copies, checkpoints them,
and both Rust and C++ reopen with the expected two rows and unchanged label bytes.
Original hashes stay unchanged; the eight older raw-name fixtures also pass read-only
checks. This verifies checkpoint interoperability, not Rust shell or WAL recovery.
Report pointer: enum-file-roundtrip-path.txt in the external artifact folder.

STRUCT/UNION lookups use native ASCII folding; regex group/result names, VARIANT keys,
summary/repeat names and log messages retain bytes. Scalar bind-expression callbacks
can propagate native errors: typeof constructs its VARCHAR while binding; alias keeps
its argument name as bytes and constructs VARCHAR at execution. C++ direct/pruned
journeys confirm that distinction; the new Rust component tests cover the callbacks
and kernel, while full Rust SQL pruning journeys remain pending.

That stage stopped at 25 optimizer diagnostics; the next continuation closes them.
Evidence/hashes: functions-enum-checkpoint.json. Remaining native expression/value,
optimizer/execution/result/logger/parser work, public shell/corpus/file/WAL gates,
selected language clients and final lower-memory performance acceptance remain open.

The optimizer/path continuation passes **187 affected release tests**, all-target
Clippy with warnings denied, and codegen checks. Join-statistics names, late row-id
rewrites, CTE result names, STRUCT/VARIANT paths and ENUM comparisons retain bytes;
STRUCT lookup follows ASCII folding. Rewrites that create VARCHAR constants validate
at C++'s actual Value-construction boundaries. The table-file known-value callback now
accepts byte column names; downstream CSV/JSON/Parquet/runtime adapters still need the
full-build integration.

ColumnIndex and StorageIndex now own raw VARIANT field-name bytes and have native
binary decoders. Generated property structs use ByteString string properties, not a
generic byte-list encoding. **10,482 C++ path comparisons** match exact metadata and
wire round trips across leaf, nested and pushed-down-extract paths; a 42-record golden
subset is checked in. The C++ oracle seeds field objects via its native deserializer
to initialize the otherwise unused/uninitialized index scalar in field constructors.
These are component/codec checks, not public query, storage-scan or WAL acceptance.

That checkpoint stopped at execution; the following stage closes those library errors. Evidence:
optimizer-path-checkpoint.json in the external artifact folder. The full workspace,
public shell/corpus/file/WAL gates, clients and final performance acceptance stay open.

The execution continuation passes **170 affected release tests** (the same pre-existing
exhaustive cast-grid test remains ignored), scoped all-target Clippy with warnings
denied and codegen checks. Execution and its targets type-check. Constraint/ALTER/DDL
names and errors, cast source-column context, CTE/window/pivot/copy metadata, scan
projections and physical plan labels retain bytes. Export SQL is written as bytes and
Value SQL-formatting errors propagate. EXPLAIN and profiling rendering return Result;
full public profiling/SQL timing and error journeys remain unverified.

The operator logger owns byte parameters and constructs checked VARCHAR Values at the
C++ boundary. Normal hash-join logging paths propagate Result; cleanup Drop explicitly
ignores its result. Parquet row-group logging helpers were adjusted to propagate
errors too; Parquet still awaits full-build integration. **3,494 C++ physical projection/
log comparisons** match exact parameter bytes, text or raw exceptions, including 140
expected invalid-Unicode errors; a 16-record independent golden subset is checked in.
These use real physical projection objects, not mock operators or a C++ engine fallback.

The serial release build took 10m52s and was resumed through the same live process,
without restart/overlap. That stage stopped at 25 CSV diagnostics; the following stage
closes those library errors. Evidence: execution-native-checkpoint.json in the external
artifact folder. No new public CLI/C API/client or full corpus/WAL/performance acceptance
is established.

The CSV continuation now compiles all targets and passes scoped Clippy with warnings
denied, codegen checks and **two release integration tests** (104 frozen C++ records).
Column names, supplied column/type maps, COPY names, multi-file union names and schema
errors retain bytes. Cast/sniffer errors and option/prompt buffers retain native names.
Option keys use ASCII matching; header collision folding now follows ASCII semantics.
`ToNamedParameters` propagates checked VARCHAR construction errors, while reject-table
append buffers retain bytes like C++'s direct string_t appends.

**12,228 exact C++ outcomes pass**: 10,482 metadata/error/VARCHAR-boundary comparisons
(six operations × 1,747 names), plus 1,746 real file scans through C++'s table-function
relation and Rust's direct read_csv binder/scanner. All scan values match; 140 expected
invalid-Unicode exceptions from ToNamedParameters match exactly. One empty-name public
C++ scan is preserved separately: its public binder generates C0, whereas the direct
Rust reader SDK exposes the original empty name. It is excluded from the SDK comparison;
public Rust SQL must verify that binder boundary. This is not complete public CSV/shell
acceptance. Native filesystem paths, raw parser ingress and full runtime/reject-table
journeys remain open.

The whole-workspace check at that checkpoint stopped at 213 autocomplete diagnostics.
Evidence: csv-native-checkpoint.json, csv-name-reference-sdk.tsv,
csv-workspace-check-1.jsonl in the external artifact folder. The following core stage
advances type/SHOW names; autocomplete remains unverified.

The unbound-type/SHOW continuation passes **138 scoped release tests**, none ignored,
across types/AST/parser/planner/CSV. All five packages pass all-target Clippy with warnings
denied, and codegen --check passes. UnboundTypeInfo, TypeParameter, TypeExpression and
ShowRef name fields retain bytes; borrowed aliases explicitly serialize as STRING bytes.
SHOW's generated VARCHAR literals validate at the actual C++ value boundary.

**34,940 C++ comparisons pass**: ten modes × 1,747 names × legacy/current serialization.
They compare exact wire bytes, native SQL text and raw binding errors, covering type
names/qualifiers/field aliases, ENUM parameters, direct default binding and SHOW fields.
A 300-record golden subset is checked in. A real legacy-default-binding difference was
fixed: an empty parameter alias uses ParsedExpression::GetName, which falls back to
native expression SQL. Direct C++ verification confirms mixed explicit/empty aliases
produce STRUCT("INTEGER" INTEGER, b VARCHAR); the old test expecting rejection now
asserts that exact structure. The initial failing test log is preserved.

This is metadata/component verification, not full type-rendering parity. The old
LogicalType::to_bytes unbound case and infallible rendering wrappers still need native,
fallible integration. Raw SQL parser ingress and public SHOW/SQL/shell journeys remain
open. Autocomplete candidate/result byte carriers and PEG name integration are WIP:
the fresh all-target check stops at **73 transformer diagnostics**; later workspace
packages remain unchecked by a successful full build. Do not decode native names to
satisfy old text signatures. Evidence: unbound-show-native-checkpoint.json and
autocomplete-native-check-7.jsonl in the external artifact folder.

The low-memory cache policy is unchanged. Python and all other selected clients, final
corpus/shell/file/WAL verification and documented final performance acceptance remain open.

Autocomplete now passes its all-target compile check, and selected autocomplete/types/
AST/parser/planner all-target Clippy with warnings denied passes. Its original 73
transformer errors are resolved through native identities and genuine PEG-token encoding,
without decoding catalog names to text. FunctionExpression/WindowExpression qualifiers
and names, CollateExpression/StringTypeInfo collation and LoadInfo metadata now retain
bytes. Function/window dispatch and collation resolution use ASCII byte semantics;
actual VARCHAR construction in USE/PRAGMA/ORDER BY propagates validation errors.

This continuation is **not yet runtime/oracle verified**. The signed C++ probe recorded
20,964 function/window/postfix/collation/LOAD outcomes, but the matching Rust comparison
harness and new release tests remain to run. PEG VersionNumber uses its native
QualifiedName rule; replacement-name dispatch remains separate from text option maps.
A one-line optimizer caller now passes its genuine function text as bytes.

The latest whole-workspace/all-target check passes autocomplete and optimizer, then
stops at **53 Parquet diagnostics**. Execution and later public/client consumers are not
validated by a successful final build. Current evidence/resume details:
autocomplete-native-wip-checkpoint.json, autocomplete-native-clippy-6.log and
autocomplete-workspace-check-2.jsonl in the external artifact folder. The prior 138-test
and 34,940-outcome results predate these additional shared AST changes and must be
rerun after this integration. Full public SQL/shell/corpus/storage/WAL, Python and other
selected clients, and final documented speed-memory acceptance remain open.

Detailed stage logs, exact limitations, source hashes and resume pointers live in
`.context/identifier-bytes-progress.md` and the external artifact folder
`.context/artifacts/identifier-bytes-20261023/`. Do not launch agents or overlap heavy
workloads to finish this integration.

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
  CLI pass (`scripts/shell-tests.sh --binary rust|cpp`), and 764 of 768 cases of
  `scripts/shell-diff.py` (modes, types, dot commands, options, errors, pty sessions)
  match byte for byte; the four others are known engine differences (work-queue item 3).
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
  the interpreter). The owner accepts this lower-memory tradeoff as the target baseline; final-binary
  performance acceptance remains open. Persistent table caching is rejected under the
  2026-10-23 policy, and the uncached source behavior is restored.

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
performance suites are complete with 160 passing result checks. Final-binary performance acceptance
remains open under the lower-memory policy in `docs/performance-acceptance.md`; that
policy supersedes the original independent speed and memory targets.

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

## Historical cache experiment (2026-10-22; rejected 2026-10-23)

Current policy: retain the uncached lower-memory baseline. The uncommitted cache switch
has been removed; its patch/source and uncached binary identities are preserved under
`.context/artifacts/performance-policy-20261023/`. The following investigation is a
historical log. Statements below about an active/uncommitted cache candidate describe
that earlier experiment, not the current source policy.

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

Shell ingress now keeps byte-owned lines, SQL accumulation, echo and linenoise
history/completeness checks. Dot-command tokenization decodes escapes as bytes and
terminates each decoded argument at NUL, like C++ C strings. A byte callback interface
carries literal `.print` output without decoding; raw command-name lookup/errors also
retain bytes. Engine errors use the native byte formatter. Valid UTF-8 still uses the
existing highlighter; invalid-byte highlighting remains plain raw output until that
text-only boundary is ported. Other dot callbacks explicitly adapt to their existing
text state; SQL execution and argv still have legacy UTF-8 handoffs. This does not
establish raw SQL/parser/name/argv parity and adds no blanket UTF-8 SQL rejection.

The ingress step passes 14 shell and 23 common signed release unit tests plus
workspace/all-targets Clippy. All 778 cases in `scripts/shell-input-bytes.py` match C++
(all single-byte raw/escaped arguments and command names plus selected sequences).
All 768 shell differential cases have zero unexpected differences and four known;
392 upstream shell tests pass with three shared skips. `input-latin1-dot-print` is
now unmarked and passes; three additional unmarked cases cover octal/NUL handling,
quoted raw input and raw command-name errors. The full SQL corpus was not repeated
for this frontend step; the last accepted full sweep remains the callback-byte run.
Artifacts: `.context/artifacts/shell-input-bytes-20261023/verify-path.txt`.

Fresh C++ file probes expose the persisted-name boundary: raw table, column, schema
and enum-type names can be written/checkpointed/reopened by C++, while Rust rejects
all four with `Serialization Error: Failed to deserialize: string is not valid UTF-8`.
The corresponding valid-UTF-8 name controls pass on both engines, and every read-only
file hash stays unchanged. Fixtures/report: `shell-input-bytes-20261023/raw-name-files/`
under the external artifact root. The binary deserializer already has raw string-byte
access; typed name storage and its UTF-8 adapter need the remaining port. These
fixtures must be handled through actual byte-owning identifiers, without encoded
placeholder names or globally rejecting invalid SQL.

The typed byte-string codec foundation is verified separately. `ByteString` owns raw
bytes with checked UTF-8 access and no implicit text display. Binary properties and
in-memory JSON documents preserve those bytes; normal JSON text output retains C++'s
UTF-8 rejection. All 271 C++ property/default-property/JSON comparisons match, including
every single byte, embedded NULs and long strings. All 25 common and six serde unit
tests pass, as do workspace/all-targets check and Clippy with warnings denied. Evidence
and source hashes: `.context/artifacts/identifier-bytes-20261023/foundation-status.json`.
Actual parser/AST/catalog names are not migrated by this foundation; the four raw-name
file gaps remain open. Continue with persisted column names and their query-result path.

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
   Keep the rejected caching policy removed and preserve the lower-memory baseline;
   evaluate speed and memory together and document per-query outliers. Complete the
   Python/other client gates in `docs/language-bindings.md`. Work
   down the rest of `docs/work-queue.md` sequentially. ICU verification is complete; Parquet encryption (15) remains.
2. Keep one workload at a time. Use `JOBS=1` for canonical conformance runs; do not
   force `SLT_THREADS=1` globally because tests of thread defaults and parallel plans
   intentionally depend on the normal setting. It remains useful for focused diagnostics.
3. TPC-DS is ported (`crates/tpcds`, all 25 files pass). Its `dsdgen`, like TPC-H's
   `dbgen` in `crates/tpch`, carries TPC's license, not MIT — the owner should review the
   TPC terms before rustdb is published; both stay optional extension crates so the core
   is MIT.
4. Complete final performance acceptance against the approved lower-memory baseline,
   with result checks and per-query outlier reporting; exact C++ speed/memory parity is
   not required. Complete differential fuzzing and all selected language-client gates
   before M9 sign-off in `docs/porting-plan.md`.
