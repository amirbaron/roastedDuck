# azure: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-azure/tree/003214c96d0caa39d5c3e27a9e1976a0692c7d37

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/azure.cmake`.
The adjacent `upstream.json` inventories 27 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

Blob and ADLS filesystems for az://, azure://, abfss:// and abfs://; read/range/list/glob and COPY/write/commit; config, credential_chain, managed_identity, service_principal and access_token secret providers; all azure_* options registered in src/azure_extension.cpp. Match scope/authentication, retry/error and bounded-memory semantics.

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

The pinned source additionally registers managed-identity client/object/resource IDs
and service-principal certificate authentication. These are required even though
the engine's generated provider-name hints omit managed_identity. Source inventory
takes precedence over that hint table.
