# PostgreSQL transport and codecs

This crate provides roastedDuck's compatible implementation with a Rust
PostgreSQL transport, Rustls TLS, binary/text result rows and COPY streams.
It does not link libpq or adapt the private C++ extension ABI.

The runtime now owns real PostgreSQL catalogs and transaction participants.
Bundled LOAD postgres registers scans, legacy postgres_attach, postgres_query,
postgres_execute and pg_clear_cache. SQL ATTACH supports schema selection,
isolation and read-only mode. CREATE table/schema/type, DROP and INSERT/CTAS use
remote DDL and COPY; rows are never stored in native DuckDB tables as a substitute.
UPDATE/DELETE/MERGE, complete metadata listings/cache behavior, pooling, secrets
and remaining settings/authentication are still pending. This is not a completed
extension or release qualification.

Binary codecs cover numeric and temporal values, UUIDs, arrays, enum/composite
values and PostgreSQL geometric primitives. The query OID whitelist differs from
table metadata mapping. Unknown types use server-side VARCHAR/VARCHAR[] casts.
COPY's value encoder is separate from SQL query parameter encoding: the latter
preserves the pinned extension's supported types, integer widths, VARCHAR text
format and rejection of parameters in binary COPY mode. Text-mode results use
actual wire format zero through the small local tokio-postgres patch documented
in third_party/tokio-postgres/ROASTEDDUCK.md.

Connections have owned runtimes and cancellation. Exhausted row/COPY readers stay
exhausted. Streaming cursors move unread rows into the engine's existing spillable
column storage when another query needs their session. Inserts materialize input
before COPY; replacing CTAS reads its old relation before DROP/CREATE. The existing
buffer manager controls these allocations; no new memory budget is introduced.

The TLS connector implements basic SSL modes, CA/hostname verification and client
certificate loading. Further libpq option/authentication parity, including OAuth
and CRL behavior, remains required. The signed wire and interruption probes use
owned TLS/SCRAM servers and non-superuser database-owner roles.

See scripts/postgres-tests/README.md and extensions/postgres_scanner/SPEC.md for
repeatable controls and scoped receipts. The direct legacy scan/attach functions
retain upstream's treatment of enable_external_access and numeric NaN settings;
real SQL ATTACH checks external access before opening a connection.
