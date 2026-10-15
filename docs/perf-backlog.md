# Performance backlog

Known places where rustdb is slower or heavier than DuckDB 1.5.6. Every item must be
closed before M9 (no benchmark below 0.99× C++ speed, memory geomean ≤ 0.6×). Numbers are
CPU time unless noted; most were measured on a heavily loaded machine and need re-checking.

| Area | Gap | Suspected cause | Found in |
|---|---|---|---|
| Sort: VARCHAR keys | 1.6× DuckDB CPU (10M rows) | per-row decode of variable-length keys | `crates/row` (`row_bench`) |
| Sort: multi-key (int, varchar desc, double) | 1.9× DuckDB CPU | same | `crates/row` |
| Parser: tiny statements (`SELECT 1`, short SELECTs) | ~1.05–1.25× DuckDB cycles per query | per-node malloc/free vs DuckDB's arena-allocated parse tree | `crates/parser` (`parse_bench`) |
| Spill files | uncompressed, no block header | zstd encoder not wired | `crates/buffer` |
