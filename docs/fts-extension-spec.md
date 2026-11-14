# FTS: roastedDuck's compatible implementation

Status: implementation present; scoped stemming, SQL, and database-interchange
gates pass. Full conformance passes the existing ratchet with documented baseline nonpasses. Features are available in normal
builds, without a default-off feature gate. No C/C++ dependency
or private C++ extension binary loading is introduced.

## Pinned contract

DuckDB **v1.5.6**, source `069cc9f9b5`, pins
[`duckdb-fts@6814ec9a7d5fd63500176507262b0dbf7cea0095`](https://github.com/duckdb/duckdb-fts/tree/6814ec9a7d5fd63500176507262b0dbf7cea0095)
in `.github/config/extensions/fts.cmake`. The distributed `v1.5.6/osx_arm64`
FTS binary independently reports `6814ec9` in its metadata and loads in the official
1.5.6 CLI. That binary is used only by the C++ oracle.

The executable contract is `extension/fts/fts_extension.cpp` and
`extension/fts/fts_indexing.cpp` at that pin, **not** the older standalone
`indexing.sql` or current upstream main. Snowball comes from DuckDB 1.5.6's
`third_party/snowball`, whose generated sources identify version 2.0.0; exact
algorithms and aliases must follow its `libstemmer/modules.h`.

## SQL surface

- `INSTALL fts; LOAD fts;`, extension catalog visibility, and provider autoload for
  `stem`, `create_fts_index`, and `drop_fts_index`.
- `stem(VARCHAR, VARCHAR) -> VARCHAR`: default NULL propagation, case-sensitive
  algorithm names/aliases, UTF-8 Snowball transformation, and identity for `none`.
  The 27 canonical algorithms are arabic, basque, catalan, danish, dutch, english,
  finnish, french, german, greek, hindi, hungarian, indonesian, irish, italian,
  lithuanian, nepali, norwegian, porter, portuguese, romanian, russian, serbian,
  spanish, swedish, tamil, turkish. Preserve the exact unknown-stemmer error/list.
- `PRAGMA create_fts_index(table, document_id, column, ...)`: VARCHAR positional
  arguments; `*` expands VARCHAR columns in table order. Named parameters:
  `stemmer='porter'`, `stopwords='english'` (also `none` or a table name),
  `ignore` (the exact punctuation/digit regex in pinned source),
  `strip_accents=true`, `lower=true`, `overwrite=false`.
- `PRAGMA drop_fts_index(table)`: remove the generated schema with CASCADE; preserve
  missing-index, existing-index, missing-table/column, and empty-column errors.
- Generated schema `fts_<schema>_<table>` in the resolved catalog contains ordinary
  `stopwords`, `docs`, `fields`, `terms`, `dict`, and `stats` tables, a `tokenize(s)`
  macro, and `match_bm25(docname, query_string, fields := NULL, k := 1.2,
  b := 0.75, conjunctive := false)`. Preserve field filtering, NULL/no-match
  behavior, repeated query-token handling, logarithm/ranking arithmetic, and
  qualified catalog/schema resolution from the oracle.

Index creation snapshots the table. Subsequent source-table writes do not maintain
that snapshot automatically. There is no custom storage index or optimizer hook:
Rust generates the pinned SQL and uses the existing engine. Bound named PRAGMA
parameters are retained by the binder and forwarded to the query-expansion callback.

## Acceptance

Run all ten unchanged pinned files in `test/sql/fts`, including both `.test_slow`
files: `test_stemmer`, `test_indexing`, `test_indexing_and_schema`, `test_fts_attach`,
`test_issue_5936`, `test_issue_10254`, `test_issue_10281`, `issue_12330`,
`issue_13866`, and `deliminator`. Preserve original assertions, prerequisites,
failures, and skips, and record actual execution versus skipped coverage.

Additionally:

1. Compare Rust stem output bytes against C++ DuckDB 1.5.6 for **every canonical
   algorithm and every registered alias**, with language vocabularies plus empty,
   NULL, mixed case, Unicode, punctuation, embedded-NUL, and boundary inputs.
   Record corpus/source hashes and per-algorithm counts; no Unicode normalization
   or approximate stemmer substitutions. Generated Rust must be reproducible and
   retain upstream licenses. Pure Rust runtime and algorithms only.
2. Create/checkpoint an FTS database in C++, reopen/query it in Rust; create it in
   Rust and reopen/query it in C++. Compare schema, persisted macros, tokens,
   dictionary, document lengths, and ranked query results. Exercise explicit LOAD
   and normal autoload after restart, attached databases, and rebuild/drop.
3. Check normal-build registration, named-argument validation, cross-connection
   use, and failure/transaction behavior; run `scripts/conformance.sh` after every
   merge and retain the existing ratchet.

## Initial evidence

Local artifacts: `.context/artifacts/vss-fts/fts-source/` (pinned upstream),
`oracle/official/duckdb`, `oracle/fts.duckdb_extension`, and
`oracle/official-identity.json` beneath the same task directory. The official
oracle reports v1.5.6, loads FTS version 6814ec9, and stems `connections` to
`connect`. The historical source-built CLI reports v0.0.1 and rejects the official
extension's version; its failed probe remains in `oracle/identity.json` and is not
used to claim extension acceptance.

## Implementation checkpoint (2026-11-13)

The pure Rust Snowball crate is generated from
`snowballstem/snowball@896281c2d89c9347f83426d37ad9b36fc4742d5b`. Regenerating all
27 algorithms as C produces the same tokens as DuckDB 1.5.6's bundled sources;
only comments and layout differ. This pins the algorithms more precisely than
Snowball's 2.0.0 version label. The one-time upstream generator is development
verification tooling only; Cargo neither builds nor links it. Generated Rust,
its byte-oriented runtime, license, hashes, and regeneration tooling are checked in.

The signed Rust stemmer executable matches **10,819,519 output byte strings**
against official C++ DuckDB v1.5.6 / FTS 6814ec9, with zero differences. Coverage
includes all 27 complete pinned vocabulary inputs, all 86 registered names, `none`,
an embedded-NUL alias, and boundary inputs. Per-case counts and hashes are in
`.context/artifacts/vss-fts/snowball/diff-full-1/verification.json`. This accepts the
measured stemming corpus; SQL integration, upstream SQL files, cross-engine FTS
storage, and full conformance remain separate pending gates.

An oracle-only shim fallback now loads pinned FTS/VSS artifacts for unchanged
`require` statements when explicitly configured. Its seven loader checks pass
against exactly one loaded official v1.5.6 library, without unsigned or metadata
mismatch bypasses. The database-interchange fixture calibration passes 24 C++
invocations. Neither result is a Rust SQL/interchange acceptance claim.

## Verified FTS milestone (2026-11-14)

The corrected signed release passes the three FTS integration tests and the CTE
scope regression (result, genuine-correlation, and plan assertions). The unchanged
pinned upstream suite has **8 PASS / 2 SKIP on each engine**. Both skipped files
(`issue_12330.test`, `issue_13866.test`) contain `require noalternativeverify`, an
unrecognized directive in the pinned C++ runner. Their original files and raw
verdicts remain unchanged. Separately hashed supplemental copies replace only
that directive with `no_alternative_verify`; both complete SQL/assertion bodies
pass on each engine. Skips are not reported as passes.

The original `deliminator.test` now passes its negative DELIM_JOIN assertion.
The fix retains CTE scope during recursive decorrelation; no operator was renamed,
SQL rewritten, or assertion weakened. The Rust SLT backend now matches the pinned
C++ generated loader's NOT_LOADED result for absent names, while the public Rust
Database API retains its Unknown distinction.

Bidirectional persisted-index interchange passes all **24 checks**: C++ creates
and Rust reads, Rust creates and C++ reads, explicit LOAD, enabled provider
autoload (network installation disabled), ATTACH, exact table/macro/token/score
snapshots, rebuild in the opposite engine, and drop/reopen. This is actual
CROSS_ENGINE verification, not oracle-only fixture calibration.

Final-source Snowball verification repeats **10,819,519 exact byte matches / zero
mismatches**, with all 27 complete vocabularies and all 86 names plus boundary
cases. Both signed Snowball unit tests pass. Strict all-target Clippy passes for
Snowball, roastedduck, and slt. Evidence:

- `.context/artifacts/vss-fts/fts-verification/fixed-fts-1/controller.json`
  and its per-file JSONL, signed-binary hashes, and source manifest.
- `fixed-fts-1/interchange/verification.json` beneath the same parent.
- `.context/artifacts/vss-fts/fts-verification/supplemental-directives/provenance.json`.
- `.context/artifacts/vss-fts/snowball/diff-full-2/verification.json`.
- `.context/artifacts/vss-fts/fts-clippy-3.log` and Snowball's `clippy-3.log`.

Workspace-wide all-target checking also passes (`fts-workspace-check-1.log`).
`scripts/conformance.sh` also passes its existing ratchet; the raw results below retain all nonpasses. VSS implementation remains a separate next step.

## Usage

```sql
LOAD fts;
CREATE TABLE documents(id INTEGER, body VARCHAR);
INSERT INTO documents VALUES (1, 'a duck swims'), (2, 'a bird flies');
PRAGMA create_fts_index('documents', 'id', 'body');
SELECT id, fts_main_documents.match_bm25(id, 'duck') AS score
FROM documents WHERE score IS NOT NULL ORDER BY score DESC;
```

`INSTALL fts` is also accepted; no download is needed for roastedDuck's compatible
implementation. An index is a snapshot: rebuild with `overwrite=true` after source
table changes. Existing global autoload policy is preserved; explicit LOAD works
regardless of whether provider autoload is enabled.

## Full conformance (FTS commit e06dd2c4)

`JOBS=1 scripts/conformance.sh` completed with exit 0 over 4,880 files:
**4,789 PASS / 76 SKIP / 15 FAIL; zero ratchet regressions; eight newly passing FTS
files.** The passing ratchet was not weakened or rewritten. Core skip membership
matches the recorded C++ reference; the only additional skips are the two FTS
directive cases described above.

All 15 raw failures are documented in `conformance/divergences.md`: eleven private
C++ fixture cases and four tests expecting an allocation failure where Rust succeeds
within the configured limit. The private fixture INSTALL case stops at its historical
v0.0.1/v1.5.6 version mismatch, before loading. These are retained exceptions, not
newly accepted C++ ABI behavior or proof of peak-memory equivalence.

The runner signature, source commit, fixture-manifest hash, and detailed verdicts
are recorded in `.context/artifacts/vss-fts/fts-conformance-1-identity.json` and
`fts-conformance-1-verification.json`; the full raw log is `fts-conformance-1.log`.
Fixtures were copied and hash-verified under `reference-corpus` so canonical test
keys match the baseline and scratch writes remain on this workspace's SSD.
Subsequent VSS working-tree hooks are not covered by this FTS receipt.


## Final extension integration check (2026-11-14)

The final VSS-integrated candidate repeats all ten original FTS files with
**8 PASS / 2 unchanged directive SKIP**, all **24 bidirectional database checks**,
and both separately named directive-corrected SQL controls successfully.
Evidence is under `.context/artifacts/vss-fts/vss-validation-4/` and
`fts-final-directive-controls/verification.json`. Normal `INSTALL`/`LOAD` and a real
stem/HNSW query smoke also pass in `extension-install-smoke-1/verification.json`.
The Snowball source remains unchanged from its full byte-differential gate.
Full conformance after code commit `07908b18` passes with zero ratchet regressions;
see [the VSS integration record](vss-extension-spec.md#evidence-and-final-conformance)
for counts, retained failures, and separate signed-runner identities. This is a
local source milestone, not a claim about previously published artifacts.
