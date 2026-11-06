`index_0-9-1.db.gz` is DuckDB 1.5.6's unchanged `data/storage/index_0-9-1.db.gz`
fixture, written by DuckDB v0.9.1 with 64-bit pointers and vector size 2048.
It contains primary-key, unique, foreign-key and explicit ART indexes. The source
and creation SQL are preserved in upstream
`test/sql/index/art/storage/test_art_duckdb_versions.test` (DuckDB MIT license).
The compressed fixture is 3.3 KB; tests expand it only in their temporary directory.
