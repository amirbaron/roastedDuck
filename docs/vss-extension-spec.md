# VSS compatible implementation specification

Status: internal hooks implemented and scoped-tested; HNSW/SQL integration is
pending. This defines roastedDuck's compatible implementation
of the VSS extension for DuckDB **1.5.6**. It does not load official C++ extension
binaries. HNSW and its engine integration are Rust, with no C/C++ dependencies and
no USearch dependency. Features are enabled in normal builds.

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

## Internal engine integration

The current `IndexEntryState` contains concrete `Art` values; `UnboundIndex`
retains type/name/create-info but its persisted carrier uses `ArtStorageInfo`.
`PhysicalCreateIndex` rejects every non-ART type. Add internal Rust traits and an
index-type registry for bound index construction, lifecycle/mutation, scans, and
type-specific metadata. Preserve ART constraints, concurrency, WAL, and checkpoint
behavior. This is not a public plugin interface.

The optimizer currently has the `extension` optimizer name but no registration
and execution mechanism for extension rules. Add an internal rule registry with
explicit ordering and enable/disable behavior. The pinned VSS registers expression,
scan, top-k, and join rules: cosine-distance normalization; distance `ORDER BY`
plus `LIMIT`; aggregate `min_by` top-k; and eligible lateral/grouped nearest-neighbor
joins. Preserve projections, residual filters, row IDs, and transactional visibility.
`EXPLAIN` must expose the operator names asserted by upstream tests. An exact scan
with an HNSW label is not an implementation.

Implement genuine hierarchical graph construction/search in Rust. Graph layout,
random choices, and approximate neighbors need not match USearch byte for byte;
SQL correctness, the pinned tests, and measured recall remain acceptance gates.

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
- Persistence-bearing files: `hnsw_basic`, `hnsw_crud`,
  `hnsw_experimental_persistence`, `hnsw_insert`, `hnsw_insert_wal` (all `.test`),
  and `slow/hnsw_reclaim_storage.test_slow`.

The latter six remain explicit deferred obligations under the in-memory assumption.
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

The hooks' required full conformance run also completed with zero ratchet regressions. Standalone HNSW and pinned SQL
macro source are subsequent, unlinked work; none of these hook receipts accepts
HNSW behavior, recall, VSS planner rewrites, or the complete extension surface.

Full conformance for hook commit `73817683` reports **4,789 PASS / 76 SKIP /
15 documented FAIL**, exactly the same per-file statuses and skip reasons as the
FTS baseline. `scripts/conformance.sh` exits 0 without changing the passing ratchet.
Evidence: `.context/artifacts/vss-fts/hooks-conformance-1-verification.json` and
`hooks-conformance-1-results.jsonl`; the tested clients are retained under
`hooks-candidate/`. This gate does not accept later HNSW/VSS working-tree changes.
