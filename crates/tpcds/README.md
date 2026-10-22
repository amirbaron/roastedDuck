# rustdb-tpcds

DuckDB's `tpcds` extension, ported one to one: `CALL dsdgen(sf = ...)` with its `catalog`,
`schema`, `suffix`, `overwrite` and `keys` options, `PRAGMA tpcds(n)`, `tpcds_queries()`
and `tpcds_answers()`. rustdb links it statically, like DuckDB's reference build
(`LOAD tpcds`, `require tpcds` in the test runner).

`src/dsdgen` ports the TPC's data generator as DuckDB embeds it (`extension/tpcds/dsdgen`):
the random number streams, the distributions of `tpcds.idx` (embedded as is), every table
generator and DuckDB's glue that appends the rows. The tables it fills are the same as
DuckDB's, value for value and in the same order, at every scale factor. `queries/` and
`answers/` are the 99 queries and their answers at SF 0.01 and SF 1, as DuckDB embeds them
(`tpcds_constants.hpp`). `columns.rs`, `tables.rs`, `tdefs_data.rs` and `schema.rs` are
generated from DuckDB's headers.

## License

dsdgen, its distributions, the queries and the answers are part of the TPC-DS benchmark
specification. They carry the TPC's legal notice ([`LICENSE`](LICENSE)), not rustdb's MIT
license, which is why this extension is a crate of its own: the rest of rustdb does not
depend on TPC code. Review the TPC's terms before distributing a build that links it.
