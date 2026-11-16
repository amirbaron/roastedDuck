# Local driver patch

Vendored tokio-postgres 0.7.18, retaining the upstream MIT and Apache licenses.
The registry crate SHA-256 is `a528f7d280f6d5b9cd149635c8705b0dd049754bc67d81d31fa25169a93809d3`.

The raw-query change adds `Client::query_raw_text` and carries a result-format
argument into the existing Bind encoder. Existing APIs still request binary
results. Parameter formats are unchanged.
roastedDuck needs actual text-format results with mixed text/binary parameters to
match DuckDB 1.5.6's postgres_query and pg_use_text_protocol behavior. Its raw-row
reader handles the fields; conventional typed FromSql readers must not consume
this added method's results. No C/C++ code or driver dependency is introduced.

The pool reset path also clears cached type definitions and the prepared type-info
statements before DISCARD ALL, so reused sessions do not reference discarded server
statements. This is implemented in client.rs.

The client records consumed ReadyForQuery transaction status, marking outstanding
requests active. Pool reset synchronizes an outstanding response before inspecting
the status, then rolls back only an open/failed transaction. This avoids warnings
from issuing ROLLBACK on idle sessions.

The supplied-token OAuth transport adds a hidden handler to Config and supports
OAUTHBEARER discovery, same-endpoint reconnect and the final AuthenticationOk.
Token selection and issuer/error validation live in crates/postgres, matching the
libpq 18.3 bundled by the pinned DuckDB PostgreSQL extension. Config Debug redacts
the handler. The driver enforces require_auth before credentials are sent and
rejects OAuth when channel binding is required. TLS still uses the existing
connector; discovery does not bypass hostname verification. No libpq plugin,
device authorization flow or C++ extension loader is included.

A hidden per-client query observer reports actual simple and extended query/COPY
operations, including internal type lookups. Statement SQL is retained privately
and omitted from Debug; observations are deduplicated between prepare and execute.
The runtime enables stderr output through the source-compatible process-global
pg_debug_show_queries setting. Pool ping/reset temporarily suppress observation.
Clients without an observer retain their existing behavior.

Changed source files (against the registry archive): client.rs, config.rs,
connect.rs, connect_raw.rs, lib.rs, prepare.rs, query.rs, simple_query.rs,
statement.rs and to_statement.rs; oauth.rs is added. Licenses are unchanged.
