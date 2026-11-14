# VSS in-memory supplements

These are separately named tests for **roastedDuck's compatible implementation**,
with C++ DuckDB 1.5.6 plus pinned VSS `b833341c8737fd3f3558c7720cc575ae8fc82598`
as the oracle. **Status (2026-11-14): all five supplements pass on both engines.**
They supplement the twelve in-scope unchanged originals: eleven in-memory files
and the persistence-disabled rejection test. They do not replace or modify any
original, and do not turn the five deferred persistence originals into passes.

`source-manifest.json` retains SHA-256 hashes for all six originally classified
persistence-related source files, each supplement,
and every copied statement/query block and SQL body. One of those six,
`hnsw_experimental_persistence.test`, is now passed independently as an unchanged
rejection test; its hash remains in the provenance ledger. The ledger describes
source derivation, while the execution receipts below establish current coverage.
Record numbers count active
`statement` and `query` headers, excluding comments and harness directives. Original
source line ranges and output positions are recorded. Query headers, SQL, expected
rows and regex assertions in the selected records are preserved unchanged; only
separating blank lines/comments are added.

The derived supplements explicitly use `load :memory:`. They remove the original
file-backed load and `SET hnsw_enable_experimental_persistence = true`, so they run
under the normal in-memory configuration. `require noforcestorage` prevents a test
harness from silently changing their scope to a disk database. It is not a product
feature gate.

| Supplement | Selected source records | Verified live-memory scope |
|---|---|---|
| `basic_before_checkpoint.test` | `hnsw_basic.test` records 2–6 | Build over 729 vectors, genuine HNSW plan assertion, and the unchanged three-neighbor proximity assertion before any checkpoint. |
| `crud_without_restart.test` | `hnsw_crud.test` records 2–4 and 6–11 | Insert/delete around a live graph, unchanged table result, HNSW plan/result and compaction invocation in the same memory database. Records 9–11 originally follow restart; here they explicitly test the current live graph only. |
| `insert_without_restart.test` | `hnsw_insert.test` records 2–4, 6–8, 10–11 | Empty index count 0, two inserts and count 2, another insert and count 3, without restarting. Each copied block also matches the corresponding block in `hnsw_insert_wal.test`; the manifest records both provenances rather than duplicating the test. |
| `native_transaction_mutations.test` | Newly authored, not copied from upstream | Mixed insert/update/delete rollback and commit, data visibility, nearest-neighbor results after transaction completion, index counts and compaction after deletes. |
| `native_empty_null_drop_recreate.test` | Newly authored, not copied from upstream | Empty graph, NULL-array exclusion, last indexed-row deletion, index DROP rollback, CREATE rollback and recreation, with real HNSW plan/result checks. |

## Supplement omissions and five deferred obligations

All original files remain unchanged at the pinned source path.

- **`hnsw_basic.test`:** omit its file-backed load, persistence option (record 1),
  checkpoint (7), restart directive, and post-restart plan/results (8–9). The
  supplement does not validate stored graph bytes or restart reconstruction.
- **`hnsw_crud.test`:** omit the file-backed load, persistence option (1), checkpoint
  (5), and restart directive. The unchanged plan/result/compact blocks (9–11) are
  deliberately executed on the still-live in-memory index, so their original
  post-restart context is **not** covered.
- **`hnsw_insert.test`:** omit the file-backed load, persistence option (1),
  checkpoint (5), all three restart directives, and repeated survival-only count
  assertions (9, 12). Inserts originally separated by restarts are executed in one
  memory database; insertion after restart and persisted counts remain deferred.
- **`hnsw_insert_wal.test`:** the same live insertion blocks are accounted for by
  `insert_without_restart.test`. Its file-backed load, persistence option,
  checkpoint, three restarts and survival-only counts remain deferred. Its original
  source already comments that it does not actually test WAL because of upstream
  issues; this supplement grants **no WAL playback coverage**. Its vector-size
  requirement concerns WAL thresholds and is not transferred to a live-memory test.
- **`hnsw_experimental_persistence.test`:** no supplement is needed. The unchanged
  original now passes on both engines: its assertion checks rejection of disk-backed
  HNSW creation with persistence disabled. It is **in scope and passed**, not a
  sixth deferral. Its hash remains in the source manifest; this rejection result
  grants no persistent-graph implementation coverage.
- **`slow/hnsw_reclaim_storage.test_slow`:** omit the entire file from supplements.
  Its million-row setup, checkpoints, `pragma_database_size()` snapshots, repeated
  DROP/recreate loop and bounded block-count assertions form a disk-reclamation
  contract. The small newly authored DROP/recreate test does not claim disk-block
  reuse, reclamation, persistent allocator correctness, or million-vector scale.

The selected original SQL/assertions are unchanged, but their reduced execution
contexts are intentional new contracts. Do not label any supplement as an original
persistence-file pass. Retain separate engine receipts and report failures without
weakening assertions or treating skips as passes.

## Source audit

`python3 conformance/vss-memory/verify_sources.py /path/to/pinned/duckdb-vss`
checks all six original hashes and every copied/native record against the manifest.
It performs no SQL execution and grants no engine acceptance.

## Verified component execution and final conformance

Current receipts are under `.context/artifacts/vss-fts/`:

- `vss-validation-4/memory-vss.jsonl` and
  `vss-memory-validation-2/cpp.jsonl`: **5 PASS each**, with no failures or skips.
  The earlier both-engine `vss-suites-2/memory-*.jsonl` malformed-header failures
  remain preserved. Separating blank lines were repaired and provenance record
  locations/hashes updated without weakening copied SQL or assertions.
- `vss-validation-4/upstream-vss.jsonl`: **12 PASS and 5 raw FAIL** across all 17
  unchanged originals. The five failures reject unsupported persistent creation
  and remain explicit deferrals, not skips. Oracle controls are retained in
  `vss-diagnostic-1/upstream-cpp.jsonl` and `vss-slow-diagnostic-1/cpp.jsonl`:
  all 15 regular and both slow original files pass on C++.
- `vss-validation-4/controller.json` records the passed component candidate.
  Its complete signed runtime library harness passes **21 tests**, including nine
  VSS-specific tests. Separate signed receipts record **12 HNSW core tests** and
  **110 storage tests across nine harnesses**, including the corrected build-time
  retirement and concurrent publication/deletion cases. These do not turn the
  supplements into persistence or whole-workspace acceptance.

Predicate filtering deliberately expands real graph candidates until LIMIT
visible, eligible rows are found or the searchable graph population is requested.
The pinned implementation filters a fixed k set and may underfill depending on
which equal-distance rows its graph chooses. This documented SQL-semantic
improvement avoids fixture-ID or tie-order tuning, preserves unchanged filtered
query assertions, and never labels an exact fallback as graph traversal. See
[the VSS specification](../../docs/vss-extension-spec.md) for the difference,
recorded shared NULL-ordering quirks, and explicit persistence boundary.

The full declared recall matrix now **passes** in
`recall-full-3/verification.json`: **405/405 Rust ef200 cells** meet the 0.95 mean
threshold after **324,000 measurements** were adjudicated. The minimum cell mean
is **0.9741**, duplicates/128/baseline/IP/k100. Its individual minimum is 0.6;
individual zero-recall queries still occur elsewhere and remain retained. All
**5,400 query-vector guards**, **1,620 ANN profiles** and **135 exact profiles**
verify, with no candidate-validation errors. The independent raw-artifact audit is
`recall-full-3-independent-review.json`.

The completed failed `recall-full-2/` receipt remains unchanged: 136/405 required
cells failed before the graph corrections. Inputs, seeds, M/M0/ef settings, query
counts and thresholds were not changed. Duplicate routing diversity, traversal of
already-admitted query-frontier candidates, and squared-L2 link diversity for IP
improved recall while retaining raw-IP priority. The extra graph work is real:
ef bounds retained candidates, not total visits or auxiliary memory. This finite
matrix result is not a speed/RSS or arbitrary-input guarantee. The invalid earlier
scoring collection and frozen harness remain under `recall-full-1/`.

The component candidate is committed as `07908b18`. Final full conformance passes
with 4,801 PASS / 76 SKIP / 20 documented FAIL across 4,897 files and zero ratchet
regressions; all prior file statuses are unchanged. The five persistent originals
remain raw failures. See `vss-conformance-final-1/verification.json` and the
[VSS spec](../../docs/vss-extension-spec.md) for the distinct runner identity and
failure review. These results do not establish the five deferred persistence
features.
