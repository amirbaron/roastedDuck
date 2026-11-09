# roastedduck-tpcds

DuckDB's `tpcds` extension, ported one to one: `CALL dsdgen(sf = ...)` with its `catalog`,
`schema`, `suffix`, `overwrite` and `keys` options, `PRAGMA tpcds(n)`, `tpcds_queries()`
and `tpcds_answers()`. roastedduck links it statically, like DuckDB's reference build
(`LOAD tpcds`, `require tpcds` in the test runner).

`src/dsdgen` ports the TPC's data generator as DuckDB embeds it (`extension/tpcds/dsdgen`):
the random number streams, the distributions of `tpcds.idx` (embedded as is), every table
generator and DuckDB's glue that appends the rows. The tables it fills are the same as
DuckDB's, value for value and in the same order, at every scale factor. `queries/` and
`answers/` are the 99 queries and their answers at SF 0.01 and SF 1, as DuckDB embeds them
(`tpcds_constants.hpp`). `columns.rs`, `tables.rs`, `tdefs_data.rs` and `schema.rs` are
generated from DuckDB's headers.

## Generation

DuckDB generates the tables one after the other on one thread. Every table draws from
random number streams of its own, so roastedduck generates them on the connection's threads at
once, each from the streams of a new generation; `inventory` and `customer_demographics`,
whose rows draw a fixed amount from each stream, are generated in parts that skip their
streams ahead and are appended in order. The rows and their order are DuckDB's at any
number of threads: the fingerprints of all 24 tables match DuckDB's at SF 0.000001, 0.008,
0.01, 0.1, 0.5, 1, 1.5, 3 and 10 (`crates/roastedduck/tests/tpcds.rs` keeps SF 0.01).

Where DuckDB's dsdgen does not return, roastedduck does:

- A scale factor above 100000 makes DuckDB's dsdgen exit the process (`ReportErrorNoLine`);
  roastedduck fails the call with the same message.
- At tiny scale factors (DuckDB 1.5.6 hangs from 0.00001 to 0.005) no day has a sale, and
  the loop that moves the sales' date forward by each day's sales never ends. The store and
  web sales draw their dates and never read that date, so roastedduck does not keep the loop;
  the catalog sales date their orders with it, so roastedduck fails once four years of days had
  no order.

## License

dsdgen, its distributions, the queries and the answers are part of the TPC-DS benchmark
specification. They carry the TPC's legal notice ([`LICENSE`](LICENSE)), not roastedduck's MIT
license, which is why this extension is a crate of its own: the rest of roastedduck does not
depend on TPC code. Review the TPC's terms before distributing a build that links it.
