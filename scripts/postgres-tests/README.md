# Disposable PostgreSQL tests

`oracle_control.py` is the small existing read/write/rollback/join control.
`upstream.py` prepares the pinned PostgreSQL extension's original fixtures and
runs original SQL files without editing their contents or expected results.

Use the local-services Python environment (cryptography is needed for the owned
TLS certificate), with PostgreSQL 17 `initdb`, `pg_ctl` and `psql` on PATH. Put
`--out` on the external artifact volume. Each invocation owns its cluster and
stops it on exit. It never uses the user's configured PostgreSQL service.

The runner creates a non-superuser database-owner role for tests and uses a
separate administrator only for fixture provisioning/reset. Schema/data initialization
uses the original SQL and client-side COPY; no server-file role is granted.
The `postgresscanner` database is restored from the seeded template before every
original file, so one test cannot supply leftover state to another.

Required inputs are `--slt`, `--shim`, `--oracle`, `--source`, `--extension`,
`--dataset-extensions` and `--out`. The dataset-extension directory must contain
the signed, version-matched oracle TPCH and TPCDS extensions. They are explicitly
loaded with automatic download/loading disabled. The pinned source archive is
checked with `--archive` or its default path beside the extracted source.

Default execution is an oracle-only four-file cohort. Use repeated `--file` to
select originals, `--all` for the complete inventory, and `--engine both` only
when verifying a Rust implementation. The runner exits nonzero on nonpasses and
retains raw results, source/data-extension hashes and server-stop evidence.
Upstream `mode skip` directives remain unchanged and visible; they do not become
passing coverage merely because their prerequisites are supplied.

The initial four-file oracle cohort passes against PostgreSQL17.11. This qualifies
the fixture, not Rust SQL support. `wire_control.py` separately exercises the signed
Rust backend probe against an owned CA/server certificate chain and non-superuser
role. It checks the reference TLS connection, backend protocol controls and server
cleanup. Runtime scan/legacy-attach registration is now present; SQL ATTACH's
catalog/transaction/mutation integration remains pending.

`codec_control.py --probe ... --cli ... --oracle ... --extension ... --out ...`
compares 39 value and logical-type cases. Thirteen use the Rust SQL runtime; 26
use the codec driver for table/query mappings whose runtime integration is still
pending. The probe also round-trips seven parameter types through PostgreSQL.
`scan_control.py --cli ... --oracle ... --extension ... --out ...` checks 25 paired
SQL cases; `--interrupt-probe .../postgres_interrupt_probe` adds an API cancellation
check that observes the server's `PgSleep` state before and after interruption.
Both drivers require the owned TLS/SCRAM cluster and retain raw results and hashes.

Use `upstream.py --slow` to supply POSTGRES_TEST_SLOW for selected original files,
such as `test/sql/scanner/decimals.test`; `--all` also supplies it. Original SQL
and reference expectations remain unchanged.
