# PostgreSQL wire backend

This crate is the Rust transport foundation for roastedDuck's compatible
implementation. It uses tokio-postgres and Rustls with ring, without libpq or a
C++ engine adapter. The runtime's SQL registration, external catalog, transaction
and mutation integration are not implemented yet; this crate alone does not make
`LOAD postgres` available.

The current backend provides owned runtimes/connections, streaming binary rows,
parameter bytes, column/type-modifier descriptions, COPY streams, cancellation,
and basic libpq-style connection/environment/service/password-file handling.
The TLS connector implements the basic SSL modes, hostname/CA verification and
client certificate loading. Further libpq option/authentication parity, including
OAuth bearer authentication and CRL handling, remains required.

The signed `wire_probe` example exercises a real disposable PostgreSQL TLS/SCRAM
server: binary parameters (including a deliberately rejected size mismatch), exact
COPY round trips, rollback, cancellation, nested Tokio calls/runtime teardown,
SSL modes, hostname verification and a wrong-password rejection. See
`scripts/postgres-tests/wire_control.py` and the extension spec for scoped receipts.
