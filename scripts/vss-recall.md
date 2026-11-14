# VSS recall and mutation verification

`vss-recall.py` measures roastedDuck's compatible implementation against exact
C++ DuckDB 1.5.6 SQL and reports the pinned C++ VSS implementation separately.
It uses Python's standard library only. Build/sign both selected artifacts before
running it. Run one heavy job at a time and use a **new** output directory on the
workspace's external SSD.

```sh
python3 scripts/vss-recall.py \
  --oracle-cli .context/artifacts/vss-fts/oracle/official/duckdb \
  --rust-cli target/release/roastedDuck \
  --vss-extension .context/artifacts/vss-fts/vss-spec/vss.duckdb_extension \
  --out .context/artifacts/vss-fts/recall-full-001
```

The defaults are the full matrix:

| Axis | Values |
| --- | --- |
| Stored vectors | 10,000 initially |
| Held-out queries | 200 per cell |
| Distributions | uniform, clustered, repeated stored vectors with independent queries |
| Dimensions | 3, 32, 128 |
| Metrics | l2sq, cosine, ip |
| k | 1, 10, 100 |
| Search setting | default (`RESET hnsw_ef_search`), `hnsw_ef_search=200` |
| Cohorts | baseline, after insert, after update, after delete, after compaction |
| Engines | official C++ DuckDB 1.5.6 + pinned VSS; selected roastedDuck binary |

This is 324,000 measured ANN queries, 1,620 additional executed ANN plan profiles,
27,000 exact-threshold queries, and 135 exact-plan profiles across 27 serial CLI
jobs. Each cohort gets the full selected query set. Mutation stages insert 200
vectors, update 200 original vectors, delete a separate 200 original IDs, then
compact each metric's index. Diagnostic sizes scale mutation counts down to retain
enough rows. Query vectors are unchanged between cohorts; ground truth is recomputed
against the matching mutated data. Construction settings remain defaults; only
the metric is specified, with a single construction thread.

For initial harness validation, use an explicitly limited run:

```sh
python3 scripts/vss-recall.py \
  --oracle-cli .context/artifacts/vss-fts/oracle/official/duckdb \
  --rust-cli target/release/roastedDuck \
  --vss-extension .context/artifacts/vss-fts/vss-spec/vss.duckdb_extension \
  --out .context/artifacts/vss-fts/recall-diagnostic-001 \
  --vectors 200 --limit 3 --cells uniform:3:l2sq:10 --cohorts baseline
```

`--cells` can be repeated. Any reduced vector/query/cell/cohort coverage is labeled
`NOT_FULL_DIAGNOSTIC`, with `full_acceptance=false` even if it passes. Fixed seeds
and parameters are recorded; do not change them after a failed cell to hide it.

## Evidence and tie-aware recall

The script generates each input once, rounds values to IEEE754 float32, and writes
nine-significant-digit CSV values shared by both engines. Inputs, mutations,
queries, commands, binaries, extension metadata, and source script are hashed.
There are separate random streams for stored vectors, held-out queries, inserts,
and updates. Duplicate fixtures create distance ties by repeating stored vectors;
queries remain independently sampled.

SQL query arrays use nine-significant-digit **exponential** numeric tokens
(`.8e`), which bind as DOUBLE before the explicit FLOAT-array cast. CSV formatting
remains `.9g`. This avoids DuckDB's DECIMAL-to-FLOAT arithmetic path for ordinary
decimal-looking SQL literals, which can differ from VARCHAR-to-FLOAT parsing by
one float32 ULP. Before constructing indexes or measuring queries, each CLI job
checks every SQL query array for exact SQL equality with its CSV-loaded FLOAT
array. Any mismatch raises an SQL error and stops that job; there is no tolerance.
Emitted and observed guard counts are included in the receipt (5,400 guards for
the full default matrix).

ANN queries return candidate IDs only. A separate C++ CLI session uses
`disabled_optimizers='extension'` and ordinary SQL distance functions for exact
top-k cutoffs. It scores every returned candidate ID against the same C++ float32
input/query vectors. Python does not approximate distances, compare Rust scores,
or apply floating-point tolerances.

For each query, let `d` be the exact kth distance, `s` the number of rows strictly
closer than `d`, `a` the returned candidates strictly closer than `d`, and `t` the
returned candidates equal to `d`. Credit is:

```text
credit = a + min(t, k - s)
recall = credit / k
```

The comparisons with `d` occur inside C++ SQL. Candidates must have length k,
unique IDs, and IDs present in the current cohort's table. Deleted/stale IDs and
duplicates fail validation. Every Rust `ef200` cell must have mean recall >= 0.95;
the threshold uses integer arithmetic (`20 * credit >= 19 * possible_credit`).
Default-ef results and C++ VSS recall are retained separately, even when lower.
The report includes per-query candidate IDs and their C++ scores, cutoffs, tie
counts, credit, minimum recall, and nearest-rank 5th/50th/95th percentiles per cell.

One actual `EXPLAIN ANALYZE` of the measured SQL shape, using query 0, is captured
for every engine/cohort/metric/k/search-setting cell. Rust must report both the
actual `HNSW Index` binding on the `TABLE_SCAN` profiler node and
`HNSW graph traversal`, with no exact-fallback label. The official oracle likewise
reports its bound `HNSW Index` through the generic `TABLE_SCAN` profiler label;
ordinary EXPLAIN uses `HNSW_INDEX_SCAN`. Exact ground-truth profiles must show a
base-table scan and no HNSW binding. This is
representative per-cell plan evidence, **not per-query traversal instrumentation**;
the report states that distinction. No synthetic graph counters are manufactured.

## Execution and retained artifacts

Each dataset/engine job is one long-lived CLI process with an in-memory database.
The script sends a complete semicolon-delimited stdin file, never a combined
`-c` CREATE/PRAGMA batch. It reads results only after the process has exited
successfully, so it does not rely on interactive stdout flushing. `.once` writes
each plan to a safe relative path within that job's directory. Every CLI uses an
empty init file, network extension installation is disabled, and only the C++
oracle loads the pinned C++ artifact. The Rust process executes `LOAD vss`.

The output includes:

- `verification.json`: identities, scope/completion flags, emitted/observed
  counters, individual cell summaries, failures, and links to all evidence.
- `<distribution>-<dimension>/fixtures/`: shared CSVs and fixture hashes.
- Per-engine `statements.sql`, `stdout.json-stream`, `stderr.log`, and `profiles/`.
- `requests.csv` and `candidates.csv`: candidate IDs submitted for oracle scoring.
- `oracle-scoring/`: exact SQL, candidate-score output, errors, and exact profiles.
- `per-query.jsonl.gz`: detailed per-query scores/credit, including failed queries.

Processes run serially. `--timeout` bounds each job; `--max-output-mb` bounds its
raw outputs, including plan files. Summary failure examples are capped at 100;
total counts and detailed per-query validation errors remain retained. Limits
fail the run rather than truncate evidence into a pass. A
failed or incomplete matrix cannot set `full_acceptance=true`. Existing output
directories are rejected rather than overwritten. Successful diagnostics and this
recall matrix do not replace the unchanged upstream SQL suite, concurrent MVCC
tests, or the explicitly deferred HNSW persistence obligations.

## Recorded component result (2026-11-14)

The fixed full matrix **passes** in
`.context/artifacts/vss-fts/recall-full-3/verification.json`:

- 324,000 measured/adjudicated ANN queries; 1,620 engine/profile cells.
- **405/405 required Rust ef200 cells meet mean recall >= 0.95**.
- Minimum cell mean **0.9741**: duplicates/128/baseline/IP/k100, with
  19,482 credited neighbors out of 20,000.
- 5,400 exact operand guards, 1,620 executed ANN profiles and 135 exact profiles
  verify; no candidate-validation failures.

The same finite matrix also retains the default-search and oracle observations:

| Engine / search profile | Cells with mean >= 0.95 | Lowest cell mean |
| --- | --- | --- |
| roastedDuck / default | 291 / 405 | 0.8400 |
| roastedDuck / ef200 (required gate) | 405 / 405 | 0.9741 |
| C++ VSS / default | 228 / 405 | 0.4150 |
| C++ VSS / ef200 | 259 / 405 | 0.47645 |

Only the specified Rust ef200 profile is the acceptance gate. These observations
are not a general ranking of the engines across other inputs or settings.

Individual query misses are retained. The lowest-mean cell has a minimum of 0.6;
across all Rust ef200 cells the individual minimum is **0.0**, and 70 cells contain
at least one zero-recall query. The gate concerns each cell's mean, not every
query. Default search and C++ VSS results remain separate observations.

The independent read-only audit at
`recall-full-3-independent-review.json` verifies the complete cell set, raw counts,
all 27 job hashes, input/candidate/detail hashes, actual index/mode profiles, and
200-query details in the lowest-mean cell and a cell with a zero-recall query.
It confirms that inputs, fixture seed, M/M0/ef settings, thresholds and the corrected
harness are unchanged from failed full run 2.

Three graph corrections produced the improvement: defer duplicate vectors when
choosing diverse routing links; finish traversing already-admitted query-frontier
candidates after displacement from the retained heap; and use squared-L2 link
diversity for IP while retaining raw-IP candidate priority. Construction's radius
stop and the retained `max(ef_search,k)` limit remain unchanged. The work spent on
real graph traversal increases; ef does not cap frontier/visited memory or total
visits. Passing this finite synthetic matrix is not a general speed/RSS guarantee,
a per-query recall guarantee, or persistence acceptance.

Earlier evidence remains preserved:

- `recall-full-1/`: interrupted invalid scorer-input collection and frozen original
  harness; the actual `recall-float-input-probe/verification.json` confirms its
  DECIMAL/CSV operand mismatch and the correction on both engines.
- `recall-full-2/`: completed genuine failure of 136/405 required cells, with
  correct operands and no candidate-validation errors. It was not overwritten or
  reclassified as passing.
- `recall-calibration-fixed-1/`: the narrow diagnostic calibration remains separate
  from the subsequent full acceptance result.

The signed component candidate is recorded in `vss-validation-4/controller.json`
and committed as `07908b18`. Final full conformance also passes its ratchet:
4,801 PASS / 76 SKIP / 20 documented FAIL across 4,897 files, zero regressions,
and unchanged prior file statuses. See `vss-conformance-final-1/verification.json`
and the [VSS spec](../docs/vss-extension-spec.md) for the separately recorded
runner identity and all failure classifications. The recall matrix does not
establish arbitrary-input coverage or the five deferred persistence features;
the signed storage tests separately cover the recorded MVCC regressions.
