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

## Assume-role, web identity and TLS (November 15)

STS client region is now separate from the generated secret's region. The client
uses the pinned SDK default-region precedence and default base credentials;
profile/config roles and external IDs follow the extension's selection. AssumeRole
requests use900-second sessions and the pinned generated session-name shape.
Returned credentials omit expiry like the C++ provider; empty keys are rejected.

Web identity resolves each role/token override through the source's environment
and profile rules. Explicit providers use CRT's AWS_PROFILE selection and generated
uppercase UUID session names unless SQL supplies one; SQL PROFILE is not forwarded.
The default chain uses the C++ constructor's AWS_DEFAULT_PROFILE precedence and
can use environment/profile session names. Its region precedence is distinct from
the explicit provider. Missing region does not become valid merely because the
secret specifies REGION. No process environment is mutated to implement this.

All15 paired signed-STS scenarios (30 records),21 paired web-identity scenarios
(42 records) and8 bad-CA/wrong-host rejection records pass. Both engines use real
TLS to owned endpoints; every STS signature is independently checked. The original
identity files still pass unchanged on both engines. SQL31, network4/17 SigV4
requests, 5 native checks/2 signed targets (13 isolated provider cases) and
Clippy2 pass. Receipt: .context/artifacts/popular-extensions/aws-role-verification.json.
The final code cleanup only replaces the equivalent duration expression; a final
signed control verifies900 seconds. Individual receipts retain image hashes.
No locked package/version or C/C++ product dependency was added. All owned jobs/
services stopped. Failed exploratory receipts remain; no live IAM claim is made.

The original AWS count remains20/21 across retained runs, including one shared
refresh failure. Next consolidate acceptance across all five extensions: ICU's
63-file fresh inventory and HTTPFS's132 originals have not been closed by the
recent provider work. Azure27 and PostgreSQL145/7 retain their stated scopes.
Then finish remaining prerequisite/transport gaps, frozen core, platform/package
checks and PyPI/Homebrew/Core delivery. Goal remains active; no new publication.
See .context/popular-final-acceptance-next.md and .context/aws-next-control.md.

## Container and instance metadata (November 15)

ECS/container credentials now match optional session-token and invalid/missing
expiry behavior. Expired ECS credentials can create a secret but do not expose
usable key fields, as upstream. Empty access keys are rejected. Relative URI
precedence, invalid relative paths and empty relative/token-file variables are
handled without mutating process environment; the exceptional direct path applies
HTTP URI/address and header validation. The normal path retains SDK validation
and token-file handling, with the pinned one-retry policy and metadata timeout.
Malformed credential responses do not consume additional retries.

IMDS credentials use the existing Rust connector for v2 tokens, first-role lookup,
v1 fallback,400-token rejection and the pinned disabled/explicit-instance behavior.
An explicit instance chain resolves even when the default-chain metadata flag is
off. Instance-profile credentials omit response expiry like the C++ loader and
reject InternalServiceException. Default-chain ECS failure does not fall through
to IMDS. No new HTTP stack, locked package/version or C/C++ product dependency
was added.

All38 paired scenarios/76 records pass on final signed images. SQL outcomes,
credential exposure, auth headers, retry counts and timeouts are checked. Relative
URI tests use process-local mapping of fixed metadata IPs to an owned service;
no system routes/DNS changed. The original web-identity/RDS files pass unchanged
on both engines. Offline SQL31, network4 with17 verified SigV4 requests, 5 native
tests/2 signed targets (13 isolated provider cases), and strict Clippy3 pass.
Receipt: .context/artifacts/popular-extensions/aws-metadata-verification.json.
All owned jobs/services stopped; failed exploratory receipts remain.

The raw helper source suggested empty-token fallback for unreadable token files,
but the actual pinned oracle rejects those before HTTP; the implementation and
controls preserve that observed behavior. All metadata controls set an explicit
region. Legacy automatic-region, broader IPv6/profile flags and further failure/
cancellation variations remain outside this checkpoint. CREATE SECRET's region
selection ends at explicit/settings/env/profile sources; do not add an inferred
IMDS region to that path merely because the C++ client probes it internally.

Next: assume-role and web-identity profile fallback, remaining provider/PG
variations, then frozen full inventories/core, platform/packages and publication.
The original AWS headline stays20/21 across retained receipts, with one shared
unchanged-credential refresh failure. The goal remains active; no new release
or Homebrew Core acceptance. Notes: .context/aws-next-control.md.

## Session SSO refresh and profile selection (November 15)

Session SSO now follows the pinned SDK1.11.702 cache and refresh behavior in Rust.
It reads SHA1(session-name) under the configured credentials directory, refreshes
within600 seconds of token expiry, uses the cached OIDC region, and sends the
source's request fields. Registration expiry is retained but does not gate refresh,
as upstream. Failed/empty refreshes can keep a still-valid token and still rewrite
known cache fields. New tokens become active only after successful writeback;
read-only files keep the previous in-memory token. Unknown cache fields are dropped
on rewrite, dates use the pinned second-resolution representation, and rotated
refresh tokens are persisted/reused. Legacy caches remain non-refreshing.

Explicit CHAIN 'sso' without PROFILE no longer selects the default profile; the
default chain still does. AWS_DEFAULT_PROFILE takes precedence over AWS_PROFILE
in the owned profile lookup. Equal inline/session fields are accepted; conflicting
or missing session links retain the pinned legacy fallback behavior.

All25 paired scenarios (50 records) pass on final signed images. They include
refresh-window boundaries, expired tokens/registrations, rejected/empty refresh,
expired responses, write failure with valid/expired previous tokens, cache reuse,
profile precedence and legacy fallback. The original web-identity/RDS files pass
unchanged on both engines. Offline SQL31, network4 with17 independently verified
SigV4 requests, 5 native tests/2 signed targets (13 isolated provider cases), and
strict Clippy2 pass. Receipt: .context/artifacts/popular-extensions/aws-sso-session-verification.json.
No locked package/version or C/C++ product dependency was added.

All services/jobs stopped. The first experimental CLI fixture was rejected by
macOS hardening; the final fixture uses the established signed SLT oracle harness,
with the official binary unchanged. Failed exploratory receipts remain. This
is owned-service qualification, not live SSO acceptance. The retained AWS original
inventory stays20 passes plus the shared unchanged-credential refresh failure;
a final frozen full inventory is still required.

Next: ECS/container and IMDS controls, then assume-role/provider failures and the
remaining PG transport/prerequisite, core, platform/package, PyPI/Homebrew/Core
acceptance gates. Source pointers: .context/aws-next-control.md. Goal remains
active; no new public release.

## Identity originals and legacy SSO (November 15)

Both previously unqualified AWS originals (web identity and RDS) now pass unchanged
on C++ DuckDB1.5.6 and Rust. Original hashes are checked. Owned STS/SSO services
validate the operation, role/token and account/role/bearer fields. RDS's original
only creates credentials; PostgreSQL signing/consumer fixtures remain separate.
Together with the earlier18, this brings the retained original inventory to20/21
passing. The remaining unchanged-credential refresh original keeps its shared
C++/Rust failure and unchanged SQL; it is not relabeled as a pass. This is not a
fresh full21-file run or live-AWS qualification.

Rust legacy SSO now reads its SHA1(startURL) token cache beside the configured
credentials file, matching the pinned C++ SDK1.11.702 instead of the Rust SDK's
home-directory assumption. Token presence, JSON/expiry validation, SSO region,
role response and credential expiry use the real provider path. Session-based
SSO still uses the SDK path and needs separate cache/refresh qualification.

The C++ oracle uses a test-only macOS transport adapter for fixed service hosts.
DNS/socket routing and CA injection are confined to that child; no system DNS or
keychain is modified. Bad-CA and wrong-hostname controls prove rejection before
an AWS request is sent. The helper is not linked into the product. Rust uses a
loopback HTTP endpoint override; this does not qualify its AWS TLS behavior.

All7 paired SSO cache controls pass, including missing, expired, malformed, empty,
invalid-expiry and rejected tokens. Offline SQL31, network4 with
17 independently checked SigV4 requests, 5 native tests/2 signed targets
(including13 isolated provider cases), and strict Clippy1 pass. No locked package
or version was added. Receipt: .context/artifacts/popular-extensions/aws-identity-verification.json.
All owned fixtures stopped. Failed exploratory fixture receipts are retained.

Next: advanced provider matrix (modern SSO cache/refresh, explicit/no-profile
selection, assume-role, ECS/IMDS), then remaining transport/prerequisite and final
core/platform/package/publication gates. Notes: .context/aws-next-control.md.
The all-five goal remains active; no new public release or Core acceptance.

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
