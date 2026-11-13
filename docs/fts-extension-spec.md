# FTS: roastedDuck's compatible implementation

Status: specification; implementation and acceptance are pending. Features will be
available in normal builds, without a default-off feature gate. No C/C++ dependency
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
parameters must reach the query-expansion callback (currently they are discarded).

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
