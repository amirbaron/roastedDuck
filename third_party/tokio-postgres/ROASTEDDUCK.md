# Local driver patch

Vendored tokio-postgres 0.7.18, retaining the upstream MIT and Apache licenses.
The registry crate SHA-256 is `a528f7d280f6d5b9cd149635c8705b0dd049754bc67d81d31fa25169a93809d3`.

The raw-query change adds `Client::query_raw_text` and carries a result-format
argument into the existing Bind encoder. Existing APIs still request binary
results. Parameter formats and authentication/TLS behavior are unchanged.
roastedDuck needs actual text-format results with mixed text/binary parameters to
match DuckDB 1.5.6's postgres_query and pg_use_text_protocol behavior. Its raw-row
reader handles the fields; conventional typed FromSql readers must not consume
this added method's results. No C/C++ code or new dependency is introduced.

The pool reset path also clears cached type definitions and the prepared type-info
statements before DISCARD ALL, so reused sessions do not reference discarded server
statements. This remains a Rust-only client.rs change.

The client records consumed ReadyForQuery transaction status, marking outstanding
requests active. Pool reset synchronizes an outstanding response before inspecting
the status, then rolls back only an open/failed transaction. This avoids warnings
from issuing ROLLBACK on idle sessions.
