# postgres_scanner: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-postgres/tree/318dabb2474fc3789b0199301a2e661e19c1b4fc

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/postgres_scanner.cmake`.
The adjacent `upstream.json` inventories 152 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

ATTACH with postgres/postgres_scanner, remote catalogs/transactions, scans/filter pushdown, writes/DDL; postgres_scan, postgres_scan_pushdown, postgres_attach, pg_clear_cache, postgres_query, postgres_execute, postgres_binary_copy, read_postgres, postgres_configure_pool, hstore functions; postgres and rds secret types, config provider and all pg_* settings. Inventory source names below is authoritative over this prose. No libpq dependency; implement protocol and catalog integration in Rust.

## Acceptance

Run unchanged upstream SQL against C++ DuckDB 1.5.6 and Rust. Keep missing-service
prerequisites explicit; skips do not qualify a feature. Add differential controls
for auth, scope, cancellation, transactions and errors where applicable. Use real
service integration in addition to protocol fixtures. Run scripts/conformance.sh
after integration merges, then verify normal Python and Homebrew packages.
No new C/C++ dependencies, binary adapters, default-off gates or public plugin ABI.

## Status

Contract recorded; not a claim that new functionality passes acceptance.
