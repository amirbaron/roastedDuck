# VSS compatible implementation specification

Status (2026-11-14): roastedDuck's compatible implementation of VSS is implemented
and its component gates pass on candidate `vss-validation-4`: 12 unchanged original
files pass, five persistence obligations remain deferred, and all 405 required
Rust ef200 recall cells pass. **Final full conformance passes with zero ratchet
regressions.** HNSW and
its engine integration are Rust, with no new C/C++ or USearch dependencies and no
default-off build gate. This does not load official C++ extension binaries.

## Pinned contract

DuckDB 1.5.6's [extension configuration](https://github.com/duckdb/duckdb/blob/v1.5.6/.github/config/extensions/vss.cmake)
pins [`duckdb-vss` b833341c8737fd3f3558c7720cc575ae8fc82598](https://github.com/duckdb/duckdb-vss/tree/b833341c8737fd3f3558c7720cc575ae8fc82598)
with `APPLY_PATCHES`. Apply DuckDB's
`.github/patches/extensions/vss/0001-reset-storage.patch`: it renames the index's
`CommitDrop` override to `ResetStorage` in its source and header. Current upstream
`main` is not the contract.

Independent provenance: the downloaded
`https://extensions.duckdb.org/v1.5.6/osx_arm64/vss.duckdb_extension.gz`
has footer version `b833341`, engine `v1.5.6`, platform `osx_arm64`, ABI `CPP`.
Its decompressed SHA-256 is
`74536af60cf0f30842e8f5b8cd771a3ee43696a96f7933e01858272c12fcc996`.
The artifact is an oracle dependency only. Source, patch, artifact metadata, and
per-test hashes are retained under `.context/artifacts/vss-fts/vss-spec/` in
`source-manifest.json` and `artifact-metadata.json`.
The executable oracle is the official DuckDB 1.5.6 CLI (source ID `069cc9f9b5`)
under `.context/artifacts/vss-fts/oracle/official/duckdb`. The historical locally
built CLI labels itself `v0.0.1` and is not the extension-loading oracle.

## SQL surface

The source of truth is the pinned `src/hnsw/` implementation and
`src/include/hnsw/hnsw.hpp`; `crates/function/src/extension_entries.rs` is the
local registration inventory.

| Surface | Contract |
| --- | --- |
| `INSTALL vss; LOAD vss` | Register roastedDuck's compatible implementation and report its real installed/loaded state. |
| `CREATE INDEX name ON table USING HNSW (expression) WITH (...)` | Exactly one fixed-size `FLOAT[N]` key; validate type/options and reject unique/primary-key use. Preserve normal index naming, `IF NOT EXISTS`, drop, table mutation, and transaction behavior. |
| `metric` index option | `l2sq` (default), `cosine`, `ip`; match source case handling and errors. |
| Index construction/search options | Integer `ef_construction >= 1` (default 128), `ef_search >= 1` (64), `M >= 2` (16), `M0 >= 2` (defaults to twice M). Reject unknown options. |
| `PRAGMA hnsw_compact_index('qualified_index_name')` | Compact deleted nodes; preserve lookup/type failures. |
| `hnsw_index_scan` | Registered internal scan table function with no ordinary SQL binder; optimizer supplies bound table/index/query/limit data. Do not invent a user-call signature. |
| `pragma_hnsw_index_info()` | Columns: `catalog_name`, `schema_name`, `index_name`, `table_name`, `metric` VARCHAR; `dimensions`, `count`, `capacity`, `approx_memory_usage`, `levels` BIGINT; `levels_stats` LIST of STRUCT(`nodes`, `edges`, `max_edges`, `allocated_bytes` BIGINT). Structural values describe the Rust graph; graph identity is not required. |
| `vss_join(left_table, right_table, left_col, right_col, k, metric := 'l2sq')` | Pinned table macro returns `score`, `left_tbl`, `right_tbl`. |
| `vss_match(right_table, left_col, right_col, k, metric := 'l2sq')` | Pinned table macro returns `matches`, a list of score/row structs. |
| Macro metrics | `l2sq`/`l2` use `array_distance` and minimum scores; `cosine`/`cos` use cosine similarity and maximum scores; `ip` uses inner product and maximum scores. Unknown metrics error. Copy pinned SQL semantics, including the fact that `l2sq` macro scores are Euclidean distance. |
| `hnsw_ef_search` | Nullable BIGINT setting; a positive value overrides the per-index search expansion, otherwise use the index setting. |
| `hnsw_enable_experimental_persistence` | BOOLEAN, default false. Registered name/type/default are part of the SQL surface; true does not authorize persistence implementation in this milestone. |

Preserve oracle handling of NULL arrays/elements, empty indexes, dimensionality,
non-finite floats, limit/offset, duplicate values, update/delete/rollback, and
invalid options. These require measured tests rather than inferred semantics.

For SQL predicates on an HNSW scan, roastedDuck deliberately expands the real
graph candidate budget until LIMIT visible, predicate-eligible rows are available
or the searchable graph population has been requested. The pinned extension
filters a fixed k candidate set and can underfill LIMIT depending on graph ties.
The unchanged `where_clause_segfault.test` expects a full result on duplicate
vectors, while this project's contract permits a different graph. Predicate-aware
expansion preserves that SQL behavior without tuning row IDs/tie ordering or
silently substituting an exact table scan. Runtime metadata reports graph
traversal and whether predicate expansion occurred; transaction-local/stale-index
exact fallbacks remain explicitly identified. This is a documented SQL-semantic
improvement over pinned fixed-k filtering, not a claim of identical graph behavior.
The signed focused predicate regression and the unchanged upstream filtered-query
file pass on the validated component candidate; see the milestone below.

A measured NULL-ordering quirk is shared with the pinned oracle: eligible indexed
`ORDER BY distance ASC NULLS FIRST LIMIT 1` and eligible lateral nearest-neighbor
joins omit a whole-NULL vector and return the nearest indexed non-NULL vector.
Disabling extension optimization returns the NULL-vector row first. Both engines'
plans show the actual HNSW scan/join; this is not a Rust/oracle divergence for the
probed shape. `vss_join` returns the non-NULL match with optimization on or off
because its `min_by` ignores NULL distances. Evidence:
`.context/artifacts/vss-fts/vss-null-order-urzbxllw/verification.json`, with both
SQL inputs, raw outputs, and plans. This limited probe does not establish every
NULL-ordering or NULL-element case.

## Internal engine integration

`IndexEntryState` now dispatches between ART and first-party secondary indexes.
Database-scoped internal Rust index factories cover construction and lifecycle
operations; ART constraints, retained deletes, and serialized bytes remain
specialized. Non-ART persisted indexes are guarded before reconstruction and stay
unbound. Read/mutation/checkpoint binding intents preserve ordinary reads while
rejecting unsupported writes. These are internal engine hooks, not a public plugin
interface.

The optimizer runs ordered pre/post rule snapshots from the per-database cache,
including ordinary, prepared-statement, and JSON-plan paths. VSS registers
expression, scan, top-k, and eligible lateral/grouped join rules. Real HNSW graph
traversal selects candidates, followed by transaction-visible table fetches.
Static EXPLAIN metadata identifies the bound table/index; runtime metadata
identifies graph traversal, predicate expansion, or the explicit exact fallback
needed for transaction-local changes or a stale index handle. An exact scan is not
reported as graph traversal.

Graph construction/search is Rust, without USearch. Graph layout, random choices,
and approximate neighbors need not match the C++ graph byte for byte; SQL
correctness, the pinned tests, and measured recall remain acceptance gates.

## Persistence boundary and test obligations

Working owner assumption: HNSW is **in-memory only**. A C++-persisted HNSW index
stays unbound when opening its database; ordinary table reads must remain usable.
Do not reinterpret its bytes as ART, silently bind it to the Rust graph, or build
cross-engine HNSW persistence. Define and test safe mutation/checkpoint behavior
around such an unbound index before claiming those paths work. Requests requiring
persistence must fail explicitly rather than acknowledge data that is not saved.

The pinned source contains **17 SQL test files**. Run the original files against
C++ DuckDB 1.5.6 and roastedDuck, preserving raw results and failures:

- In-memory files: `hnsw_join_macro`, `hnsw_lateral_join`,
  `hnsw_lateral_join_group`, `hnsw_metrics`, `hnsw_options`, `hnsw_projection`,
  `hnsw_result`, `hnsw_rewrite`, `hnsw_topk`, `where_clause_segfault` (all `.test`),
  and `slow/hnsw_lateral_join_group_large.test_slow`.
- In-scope rejection file: `hnsw_experimental_persistence.test` checks that
  disk-backed HNSW creation fails when persistence is disabled. It runs unchanged
  and passes; this does not require or establish HNSW persistence.
- Five deferred persistence files: `hnsw_basic`, `hnsw_crud`, `hnsw_insert`,
  `hnsw_insert_wal` (all `.test`), and `slow/hnsw_reclaim_storage.test_slow`.

The latter five remain explicit deferred obligations under the in-memory assumption.
Do not edit them into in-memory tests or count skips as passes. Therefore full
upstream-suite parity cannot be claimed in this milestone. Separately verify
opening/querying a C++ database with a persisted HNSW index left unbound.
Where a mixed persistence file contains useful in-memory behavior, add a separately
named in-memory case with source-file/statement provenance; preserve and report the
unchanged original file independently.

Additional recall acceptance: use committed deterministic generators/seeds, held-out
queries, and exact SQL top-k ground truth for all three metrics; cover uniform and
clustered data, duplicates/ties, dimensions 3/32/128, and k=1/10/100. Start with
10,000 vectors and 200 queries per distribution/dimension, default construction,
and `hnsw_ef_search=200`. Require mean tie-aware recall@k >= 0.95 in each cell;
report per-query minima/quantiles, oracle VSS recall, and the default-ef results as
well. Retain failed cells; do not increase settings after seeing results to hide a
failure. Also validate recall after inserts, updates, deletes, and compaction,
and check that returned rows obey snapshot visibility. This is a project acceptance
criterion, not a claim that upstream promises this threshold.

Run `scripts/conformance.sh` after every merge. Keep builds/tests serial and place
large artifacts on the workspace's external artifact directory. Existing release
receipts do not accept the new extension implementation.

## Internal hooks milestone (2026-11-14)

Implemented database-scoped internal index-type and optimizer-rule registries;
ART/secondary bound-index dispatch; generic build/publication and mutation,
rollback, retained-delete cleanup and vacuum hooks; and explicit read/mutation/
checkpoint binding intents. ART constraints and serialized bytes remain specialized.
Optimizer snapshots are discovered through the per-database cache, including the
ordinary, prepared-statement, and JSON-plan constructor paths. There is no public
plugin SDK or C API registration surface.

Scoped verification passes:

- All workspace targets compile; strict Clippy for storage, optimizer, and runtime.
- All eight signed storage harnesses: **101 PASS, zero failures or ignores**,
  including six secondary-lifecycle tests and the selective-binding regression.
- Signed optimizer-hook tests: **6 PASS**, including ordering, isolation,
  binding allocation, disabled rules, and registration during a callback.
- All **144 unchanged ART SQL files pass** on the rebuilt signed runner.
- A real C++ v1.5.6/b833341 clean checkpoint containing HNSW and ART indexes:
  **18 CLI executions validate read compatibility and rejected writes**. Rust
  opens it in read-only and ordinary modes, executes filtered/unfiltered/exact
  distance queries, and closes without changing file bytes. INSERT/UPDATE/DELETE
  and explicit CHECKPOINT fail before changing data; C++ then reopens each copy
  with its original graph intact. The old Rust binary's FLOAT[3]-as-ART open
  failure is retained as the red control. This verifies opaque/unbound handling,
  not Rust HNSW persistence or HNSW query acceleration.

Evidence: `.context/artifacts/vss-fts/vss-hooks/verification-1/` contains the source
manifest, controller, storage-test review (post-signature binary hashes), ART
verdicts, and `unbound-hnsw/verification.json`. The controller's initial
`test_binaries` hashes precede signing; use `storage-test-review.json` for the
signed test identities. `optimizer-tests-1.json`, `hooks-workspace-check-1.log`,
and `hooks-clippy-5.log` are in the task artifact directory.

The hooks' required full conformance run also completed with zero ratchet
regressions. These historical hook receipts do not accept the later HNSW/VSS
implementation; its separate scoped receipts follow below.

Full conformance for hook commit `73817683` reports **4,789 PASS / 76 SKIP /
15 documented FAIL**, exactly the same per-file statuses and skip reasons as the
FTS baseline. `scripts/conformance.sh` exits 0 without changing the passing ratchet.
Evidence: `.context/artifacts/vss-fts/hooks-conformance-1-verification.json` and
`hooks-conformance-1-results.jsonl`; the tested clients are retained under
`hooks-candidate/`. This gate does not accept later HNSW/VSS working-tree changes.


## Component acceptance milestone (2026-11-14)

`vss-validation-4/controller.json` records `candidate_gates_passed`. Its signed
candidate passes **12 of the 17 unchanged upstream VSS files**: the eleven
in-memory files, including the large lateral/grouped `.test_slow`, plus the
persistence-disabled rejection file. The other **five remain raw FAIL results at
unsupported persistent creation**, explicitly deferred by scope, not skipped or
accepted persistence tests. The official C++ oracle passes all 17 originals.
All **five separately named in-memory supplements pass on both engines**; reduced
contexts do not convert deferred originals into passes. The
[supplement provenance](../conformance/vss-memory/README.md) retains all six
originally classified persistence-related source hashes, including the rejection
file now executed unchanged.

Other component gates:

- **21 signed runtime library tests pass**, including nine VSS-specific tests for
  macro aliases, graph scan/join execution, real metadata, compaction, min-by top-k,
  prepared transaction-local changes/rollback, explicit fallbacks, selective
  duplicate-vector predicates, and measured OFFSET/NULL-element quirks. This is the
  runtime library harness, not every integration harness in the workspace.
- **12 signed HNSW core tests pass**, including duplicate routing diversity,
  admitted-frontier traversal, inner-product link geometry, and nonfinite ordering.
- **110 signed storage tests pass across nine harnesses**, including the HNSW
  wrapper, secondary hooks, ART constraints and MVCC. Build-time retirement now
  recognizes already committed deletions visible to older snapshots, and delete
  maintenance shares the index-build publication lock. The original signed red
  failure and corrected old-reader/rollback/rebuild/race cases remain retained.
- Workspace/all-target checking and strict Clippy pass for the component candidate.
- With VSS loaded, the **18-execution clean C++ HNSW/ART read/unbound check passes**.
  Enabling VSS does not deserialize or make the persisted C++ graph writable.
- FTS is rechecked on this candidate: **eight unchanged originals PASS and two
  matching original directive-related SKIPs**; the earlier two directive-corrected
  controls preserve their separate evidence. The **24-execution bidirectional FTS
  database interoperability** check passes again.

## Full recall result and retained failures

The declared matrix passes in `recall-full-3/verification.json`, with
`finished=true`, `passed=true`, and `full_acceptance=true` for this finite recall
contract. It covers 10,000 initial vectors and 200 held-out queries per cell,
uniform/clustered/duplicate distributions, dimensions 3/32/128, all three metrics,
k=1/10/100, default and ef200 search, both engines, and baseline/insert/update/
delete/compact cohorts.

- **324,000 ANN measurements adjudicated**, covering 1,620 engine/profile cells.
- **405/405 required Rust ef200 cells have mean tie-aware recall >= 0.95**.
  The lowest mean is **0.9741**, duplicates/128/baseline/IP/k100 (19,482 credited
  neighbors out of 20,000).
- **5,400/5,400 exact query-vector guards**, **1,620/1,620 ANN profiles**, and
  **135/135 exact profiles** verify; no candidate-validation failures.
- Individual misses remain visible. The lowest-mean cell has an individual
  minimum of **0.6**; across the full Rust ef200 matrix the individual minimum is
  **0.0**, with at least one zero-recall query in 70 cells. Acceptance is a
  per-cell mean threshold, not a guarantee for every query. Default-ef and C++
  reference results are reported independently, not forced through the Rust gate.

The first corrected full run, `recall-full-2/`, remains complete failed evidence:
136 of 405 required cells were below threshold, with no scorer-input errors.
Three core corrections improved the graph without changing M, M0,
ef_construction, ef_search settings, fixture seeds, query counts or thresholds:

1. Equal vectors no longer consume every routing-diversity slot; remaining
   capacity can still hold duplicate labels, preserving distinct SQL rows.
2. Queries finish traversing candidates already admitted to the frontier even
   after they leave the retained nearest set. Construction's radius stop and the
   retained `max(ef_search,k)` bound remain unchanged.
3. Inner-product search keeps raw IP candidate priority, while link diversity
   compares squared-L2 separation from selected neighbors versus the center.

These changes spend more real graph work. The retained heap bound is not a bound
on frontier size, visited nodes, latency, or RSS; no speedup or general memory
acceptance follows from recall passing. Graphs and returned tied IDs need not be
identical to the oracle. The result establishes this measured finite-input matrix
on the recorded platform, not every distribution, dimension, nonfinite input,
query, or persistent-index workload.

`recall-full-1/` remains invalid harness evidence: DECIMAL-looking query literals
and CSV query operands differed. Its frozen script/interruption record remain
unchanged. The corrected exponential-literal harness and exact SQL operand guards
were unchanged between full runs 2 and 3; no tolerance or score normalization was
introduced. The narrow successful calibration remains diagnostic evidence only.

## Evidence and final conformance

Paths below are relative to `.context/artifacts/vss-fts/`:

- `vss-validation-4/` contains `controller.json`, `source-manifest.json`,
  runtime/check/lint logs, and `upstream-vss.jsonl`, `memory-vss.jsonl`,
  `upstream-fts.jsonl`, recording the component candidate and original raw outcomes. Its Rust CLI SHA-256 is
  `d63ab39062200623d88a960bb56ff452f41c4b20ba0a3a16cc10bb50aacda68f`;
  SLT SHA-256 is
  `0defd31985daf06932caf5ea677c593964dba203506c3acbc8f2e48c30b063c1`.
- `vss-core-recall-fix-validation/verification.json` and
  `mvcc-build-retirement/green-verification.json` retain signed core/storage
  identities, source hashes, and their focused regression evidence.
- `vss-validation-4/unbound-loaded/verification.json` and
  `vss-validation-4/fts-interop/verification.json` record the current cross-engine checks.
- `vss-diagnostic-1/upstream-cpp.jsonl`, `vss-slow-diagnostic-1/cpp.jsonl`, and
  `vss-memory-validation-2/cpp.jsonl` retain unchanged oracle controls. Earlier
  failed collections remain preserved rather than rewritten as passes.
- `recall-full-3/verification.json` retains every cell and points to raw SQL,
  outputs, profiles, candidate scores and per-query details.
  `recall-full-3-independent-review.json` independently verifies all 1,620 cell
  keys, 27 job hashes, fixture/candidate/detail hashes, raw measurement/guard
  counts, actual index/mode profiles, and 200-query detail records in both the
  lowest-mean cell and a cell containing a zero-recall query. It also confirms
  unchanged inputs, seed, parameters, threshold and harness versus failed run 2.

Ordinary EXPLAIN names the HNSW scan; EXPLAIN ANALYZE uses its generic `TABLE_SCAN`
profiler label with the actual `HNSW Index` binding, plus Rust's explicit graph
execution mode. Profile evidence is one executed query per cell, not per-query
graph-work instrumentation.

Final `JOBS=1 scripts/conformance.sh` on code commit `07908b18` exits 0:
**4,897 files: 4,801 PASS / 76 SKIP / 20 documented FAIL**, with **zero ratchet
regressions**. All 4,880 prior file statuses and skip reasons match the hook
baseline. The existing ratchet is unchanged. Its 20 newly passing extension files
are recorded separately because their pinned fixtures are staged outside the
stock core corpus.

The 20 raw failures are the same eleven private C++ fixture cases and four
expected-OOM queries that succeed, plus the five explicitly deferred HNSW
persistence originals. Baseline failure lines/messages also match, allowing only
worker IDs in temporary paths when comparing the unchanged raw receipts.
All 144 ART SQL files pass. Evidence is in
`vss-conformance-final-1/{verification.json,rust-run.jsonl,failure-review.json}`
and its full log. The original SQL files and failure outputs are retained.

The required script signs the SLT runner again. The final runner's SHA-256 is
`bddf119d9b7496460bc746a600f0b5affa1389526dc52d60d96531d66be2af84`;
its signature verifies and every Mach-O section matches the retained Cargo
compiler artifact (`macho-section-review.json`). Earlier component receipts keep
their original signed-runner hash. The recall/interoperability CLI remains
byte-identical, and no Rust/Cargo source changed after `07908b18`.
The two directive-corrected FTS SQL controls also pass on this final runner;
see `fts-final-directive-controls/verification.json`. Their originals still count
as two skips, not passes.

This completes the declared local source milestone with in-memory HNSW. It does
not accept the five deferred persistence obligations or publish a new release.
