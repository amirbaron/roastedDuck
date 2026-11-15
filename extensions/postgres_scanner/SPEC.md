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
for auth, scope, cancellation, transactions and errors where applicable. Use local emulator/disposable-service
integration in addition to protocol fixtures, as authorized by the owner on
November 14. Record live-cloud behavior separately as unverified until exercised. Run scripts/conformance.sh
after integration merges, then verify normal Python and Homebrew packages.
No new C/C++ dependencies, binary adapters, default-off gates or public plugin ABI.

## Status

Contract recorded; not a claim that new functionality passes acceptance.

The pinned `database-connector` submodule is
`0a8505f775dae7bb30edd37f848d5975bf72884e`. A basic reference fixture is executable
with `scripts/postgres-tests/oracle_control.py`. It creates a fresh, loopback-only
PostgreSQL cluster under the supplied artifact directory, verifies remote reads,
writes, rollback, query pushdown and a local/remote join, then stops the server.
The first run passes on C++ 1.5.6 and PostgreSQL 17.11; Rust was not tested because
the connector is not implemented. This does not replace the 152 original SQL
files or pool/TLS/authentication/concurrency acceptance.


## Wire backend checkpoint (November 15)

`crates/postgres` adds a Rust transport foundation using tokio-postgres0.7.18 and
Rustls/ring. It does not link libpq or implement a C++ engine adapter. The backend
has streaming binary results and COPY, raw parameter bytes, column OIDs/type
modifiers, owned runtimes and query cancellation. Connection/environment/service/
password-file handling and basic TLS modes are present. SQL registration and
remote catalog/transaction/mutation hooks are not yet integrated; this is not
`LOAD postgres` support or completed compatibility acceptance.

The signed wire probe passes against a disposable TLS/SCRAM PostgreSQL17.11 server
using a non-superuser database-owner role. It verifies actual TLS, binary values,
server rejection of a mismatched binary parameter, exact COPY round trips,
rollback, cancellation, operation and teardown within another Tokio runtime,
SSL modes, CA-versus-hostname verification and wrong-password rejection. The C++
TLS control passes on the same server. Strict all-target backend Clippy passes.
The dependency audit preserves all existing locked versions;41 packages are added,
without new native library packages. Transitive WASM updates were pinned back.

The reusable fixture restores a seeded postgresscanner database before each file.
TPCH/TPCDS generators are explicitly loaded from owned, signed1.5.6 oracle artifacts
with automatic loading/downloading disabled. Original setup SQL is checked against
the pinned archive; client-side COPY avoids server-file privileges. Four unchanged
originals (types, arrays, SSL, uppercase-name creation) pass on C++ with the final
CA/server chain; servers stop after both runs. Tests remain unmodified, including
upstream mode-skip directives.

Receipts under `.context/artifacts/popular-extensions/`: postgres-wire-control-2,
postgres-upstream-fixture-2, postgres-dependency-audit.json,
postgres-backend-clippy-final-2.log, and postgres-dataset-oracle-1.
Still required: SQL/catalog/transaction/writes, full codec/options/pool behavior,
OAuth/CRL and remaining authentication parity,152 originals and supplemental
controls, full integration conformance and package/platform/publication gates.
