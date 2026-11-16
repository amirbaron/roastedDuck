# Local extension integration fixtures

No AWS or Azure account is needed for these development checks. They use Moto
for S3 and STS, Azurite for Azure Blob Storage, and an isolated real PostgreSQL
server for the reference connector checks. All services bind to loopback and stop
after each run, including failed checks. Credentials are fixed, disposable test
values. The fixture tools are not dependencies of the shipped Rust engine.

## Setup and run

From the repository root, install the pinned development tools:

```sh
python3 scripts/local-services/setup.py
```

In local Conductor workspaces, setup verifies the external SSD mount and uses the
workspace's dedicated artifact directory. It does not install global Python or
Node packages, start Docker, or change existing database services. If the artifact
link conflicts, use the absolute tools path printed by setup in the commands below.

Run against a CLI built from the current branch:

```sh
.context/artifacts/local-services/venv/bin/python scripts/local-services/check.py \
  --tools .context/artifacts/local-services \
  --out ".context/artifacts/local-services/run-$(date +%Y%m%d-%H%M%S)" \
  --cli target/release/roastedDuck
```

Omit `--cli` to check only the storage emulators. Each output directory must be
new. Keep generated data, logs, caches and virtual environments on the artifact
store. Setup refuses to replace tools while a fixture run is active.

The `Services` context manager in `check.py` can also wrap additional integration
tests. It exposes S3/Blob endpoints and a subprocess environment with a generated
`roastedduck-fixture` AWS profile, fake credentials, a local endpoint, and EC2
metadata disabled. Use `service.environment` for child processes; do not dump the
entire environment into logs because it also inherits ordinary parent settings.
The endpoints exist only for the context's lifetime.

## C++ oracle and PostgreSQL

For the pinned DuckDB 1.5.6 oracle used in this workspace, add:

```sh
  --oracle .context/artifacts/vss-fts/oracle/official/duckdb \
  --httpfs-extension .context/artifacts/popular-extensions/oracle/httpfs.duckdb_extension \
  --azure-extension .context/artifacts/local-services/oracle/azure.duckdb_extension \
  --postgres-extension .context/artifacts/popular-extensions/oracle/postgres_scanner.duckdb_extension
```

These are reference-engine artifacts, not files loaded by roastedDuck. Oracle
mode checks S3 and Blob CSV reads/Parquet writes and invokes
`scripts/postgres-tests/oracle_control.py`. That script requires local `initdb`
and `pg_ctl`, creates its own data directory and random loopback port, checks
attach/read/write/rollback/query/join behavior, and stops its server.

## Evidence and limits

Verified November 14, 2026 with Moto 5.2.5, Azurite 3.37.0, Azure Python Blob SDK
12.31.1 and PostgreSQL 17.11. Receipt:
`.context/artifacts/local-services/check-final/verification.json`.

Eight grouped checks pass: S3 put/get/range/list, simulated STS credentials,
Blob put/get/range/list, roastedDuck S3 SQL read/write, C++ S3 SQL read/write,
C++ Azure SQL read/write, storage deletion and the C++ PostgreSQL contract.
The receipt records executable hashes, tool versions, SQL and raw results.
Earlier harness failures are retained in `check-1/` and `check-2/`; a preliminary
successful run is in `check-3/`.

This verifies local integration, not production cloud behavior:

- [Azurite does not emulate ADLS Gen2](https://learn.microsoft.com/azure/storage/common/storage-use-azurite).
  ADLS protocol fixtures still need to be added.
- [Moto server mode](https://docs.getmoto.org/en/latest/docs/server_mode.html)
  simulates AWS APIs. Its acceptance of a request is not independent proof of
  correct SigV4 authentication or real IAM policies; keep the existing signed
  request protocol controls as well.
- Actual IAM/Entra identity, managed identities, cloud policies, KMS and cloud
  network/failure behavior remain unverified.
- The Rust AWS credential-chain, Azure and PostgreSQL implementations are still
  pending. This driver does not claim to test them. The Azure/PostgreSQL checks
  above exercise the C++ reference only.

The owner authorized local emulation for development integration. Missing cloud
accounts therefore do not block implementation or the local compatibility suites.
Any later live-cloud evidence must be reported separately.
