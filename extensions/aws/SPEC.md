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

## SQL integration checkpoint (November 15)

Normal builds now register AWS SQL entry points and credential_chain providers for
s3/aws/r2/gcs. The integration supports provider/type autoload when enabled and
keeps blocking credential I/O outside the secret catalog lock. Legacy calls match
the pinned named-profile region rules. Refresh-info field order follows the
platform C++ unordered map. Default redaction follows the pinned implementation,
including inline configuration values; test credentials are always isolated fakes.

Query interruption propagates to the resolver and terminates owned credential
process groups. There is no arbitrary process-output cap or fixed total resolver
deadline; valid large process output is covered. The RDS signing backend is
implemented and matches an independently verified captured C++ token, but the
RDS SQL/type/consumer integration is not complete.

Evidence: 31 paired offline SQL controls; profile-based S3 read/write and process
refresh on both engines with 17 independently verified SigV4 requests; public Rust
API interruption and CLI process cleanup; 13 isolated provider cases, cancellation,
large-output and RDS token unit checks. Eleven unchanged metadata/profile files
pass on both engines with the real httplib setting selected via the runner fixture.
Default transport runs preserve the upstream error-message skip instead of changing
its SQL expectation. The fixture disables its identity-denial proxy for the single
localhost-refusal test, which must actually attempt that local connection.

Remaining: all other original AWS files and advanced network providers, RDS SQL
and PostgreSQL integration, cross-platform/native package qualification, full
conformance and publication. This checkpoint is not full AWS acceptance.

## Local original data/environment/proxy tests (November 15)

Eighteen of 21 unchanged original files now pass on both engines: the eleven
metadata/profile cases above, three S3 data cases, two environment cases and two
proxy cases. The local S3 fixture independently verifies SigV4, rejects malformed
regions and invalid keys, and starts fresh storage for each engine. The proxy
fixture restricts forwarding to that loopback service and enforces Basic auth
for the authenticated original. No cloud accounts or real credentials are used.

`aws_secret_refresh.test` retains the same raw failure on both engines: it expects
a successful-refresh log after retrying unchanged environment credentials. The
pinned HTTPFS source logs only when credentials actually change. Identical local
403 HEAD/GET traffic is recorded for both engines. The separate credential-process
rotation control proves the real changed-credentials retry and log. The original
test has not been edited or relabeled as a pass.

Receipts: `.context/artifacts/popular-extensions/aws-upstream-data-1/`,
`aws-upstream-proxy-1/`, and `aws-upstream-env-4/`. Reproduce with
`scripts/aws-tests/upstream_{data,proxy,environment}.py --help` and the pinned
sources, signed SLT image and C++ shim. Runner-only `SLT_S3_URL_STYLE` and
`SLT_HTTP_PROXY` select ordinary SQL settings equally for both engines.
The environment script exits nonzero while the raw upstream failure remains.
Web-identity and RDS originals, advanced provider controls, other extensions,
full conformance and package/release qualification remain incomplete.
