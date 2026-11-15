# PostgreSQL wire backend

This crate is the Rust transport foundation for roastedDuck's compatible
implementation. It uses tokio-postgres and Rustls with ring, without libpq or a
C++ engine adapter. The runtime now registers `postgres_scan`,
`postgres_scan_pushdown` and legacy `postgres_attach` through bundled
`LOAD postgres`. Remote catalog/transaction/mutation integration for SQL `ATTACH`
is still pending, as is actual filter pushdown; this is partial implementation.

The current backend provides owned runtimes/connections, streaming binary rows,
parameter bytes, column/type-modifier descriptions, COPY streams, cancellation,
and basic libpq-style connection/environment/service/password-file handling.
Binary codecs cover numeric and temporal values, UUIDs, arrays, enum/composite
values and PostgreSQL's geometric primitives. The query OID whitelist is separate
from catalog type mapping. Unknown scalar types and array elements use the pinned
server-side text casts. `encode_parameter` follows the input DuckDB type, preserving
server errors for incompatible inferred parameter types.
The TLS connector implements the basic SSL modes, hostname/CA verification and
client certificate loading. Further libpq option/authentication parity, including
OAuth bearer authentication and CRL handling, remains required.

The signed `wire_probe` example exercises a real disposable PostgreSQL TLS/SCRAM
server: binary parameters (including a deliberately rejected size mismatch), exact
COPY round trips, rollback, cancellation, nested Tokio calls/runtime teardown,
SSL modes, hostname verification and a wrong-password rejection. See
`scripts/postgres-tests/wire_control.py` and the extension spec for scoped receipts.

`codec_control.py` compares value rendering and logical types to C++ over a real
owned server. `scan_control.py` checks SQL scans, legacy view creation and optional
API interruption, including observing the server leave `pg_sleep`. The small codec
test target also exercises malformed lengths, array shapes and query type mapping.

Two pinned legacy behaviors are intentional: these direct scan/attach functions
do not check `enable_external_access`, and legacy scans keep numeric NaN-as-NULL
even when `pg_numeric_nan_as_null=false`. SQL `ATTACH` has its own access checks;
its implementation remains required. See the source-pinned extension spec.
