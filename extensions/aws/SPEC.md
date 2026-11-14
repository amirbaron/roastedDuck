# aws: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-aws/tree/28c853c084a6e3acd36d7b8018c42438bb8c5a33

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/aws.cmake`.
The adjacent `upstream.json` inventories 21 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

Credential-chain providers for s3, aws, r2, gcs and rds; explicit chain/profile, config/env/process/STS/SSO/instance/web identity, override parameters, validation, refresh and redaction; load_aws_credentials with optional profile and set_region/redact_secret arguments. Match the pinned SDK-facing behavior with Rust code, including expiry, failed refresh and persistent secrets.

## Acceptance

The pinned vcpkg baseline resolves aws-sdk-cpp 1.11.702. The default constructor's
provider order is environment, profile, process, web identity, SSO, then container
credentials or instance metadata. Its configured constructor also includes login
credentials; the extension's default call does not use that constructor. Preserve
this distinction instead of adopting a newer Rust SDK's default chain unchanged.
The reference provider source is retained as `aws-provider-chain.cpp` in the
external artifact directory; it is not shipped or compiled into roastedDuck.

Run unchanged upstream SQL against C++ DuckDB 1.5.6 and Rust. Keep missing-service
prerequisites explicit; skips do not qualify a feature. Add differential controls
for auth, scope, cancellation, transactions and errors where applicable. Use local emulator/disposable-service
integration in addition to protocol fixtures, as authorized by the owner on
November 14. Record live-cloud behavior separately as unverified until exercised. Run scripts/conformance.sh
after integration merges, then verify normal Python and Homebrew packages.
No new C/C++ dependencies, binary adapters, default-off gates or public plugin ABI.

## Status

Contract recorded; not a claim that new functionality passes acceptance.

## Backend implementation checkpoint

`crates/aws` now implements a Rust credential-resolution backend with explicit
pinned provider ordering, role/profile validation, region fallback and redacted
credential debug output. Twelve isolated Rust provider cases and strict Clippy
pass; receipts are under `.context/artifacts/popular-extensions/aws-*`.
The dependency audit adds no new C/C++ crypto or database client library and
preserves every existing locked package version.

This is not SQL support or completed compatibility acceptance: engine binding,
legacy table-function behavior, RDS signing, process cancellation, network-provider
differentials, all 21 originals and platform/package qualification remain open.
