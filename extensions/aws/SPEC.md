# aws: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-aws/tree/28c853c084a6e3acd36d7b8018c42438bb8c5a33

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/aws.cmake`.
The adjacent `upstream.json` inventories 21 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

Credential-chain providers for s3, aws, r2, gcs and rds; explicit chain/profile, config/env/process/STS/SSO/instance/web identity, override parameters, validation, refresh and redaction; load_aws_credentials with optional profile and set_region/redact_secret arguments. Match the pinned SDK-facing behavior with Rust code, including expiry, failed refresh and persistent secrets.

## Acceptance

Run unchanged upstream SQL against C++ DuckDB 1.5.6 and Rust. Keep missing-service
prerequisites explicit; skips do not qualify a feature. Add differential controls
for auth, scope, cancellation, transactions and errors where applicable. Use real
service integration in addition to protocol fixtures. Run scripts/conformance.sh
after integration merges, then verify normal Python and Homebrew packages.
No new C/C++ dependencies, binary adapters, default-off gates or public plugin ABI.

## Status

Contract recorded; not a claim that new functionality passes acceptance.
