# Five most-downloaded extensions: roastedDuck implementation assessment

Assessment date: November 14, 2026. This is planning research, not new feature
delivery or a full upstream-source audit. Compatibility target remains DuckDB
1.5.6; current 2.0 documentation must not silently expand that contract.

## Selection and evidence

Use downloads rather than GitHub stars. Both official endpoints were fetched
directly on November 14, with `_last_update` values
`2026-11-14T04:00:24.355Z` (core) and `2026-11-14T04:00:27.049Z`
(community). Raw responses are retained in
`.context/artifacts/extension-popularity-review/{core,community}.json`.
The web search cache returned older snapshots; the counts below use the direct
responses. Counts aggregate versions/platforms, are not unique users, and
underrepresent extensions already bundled in clients. Dependency downloads also
affect rankings. This is a weekly ranking, not an all-time popularity claim.

| Rank | Extension | Downloads in reported week | Current source status | Remaining effort |
| --- | --- | ---: | --- | --- |
| 1 | httpfs | 19,947,614 | Rust implementation present | Gap qualification and loading UX |
| 2 | aws | 8,535,778 | Name/provider routing present; implementation missing | Medium–large credential-provider work |
| 3 | azure | 4,120,318 | Names/URI routing present; filesystem missing | Large filesystem/authentication implementation |
| 4 | icu | 3,670,219 | Built-in Rust implementation present | Regression/data-version maintenance |
| 5 | postgres_scanner (SQL alias postgres) | 3,418,629 | Names/routing present; connector missing | Very large engine and connector work |

The largest community count is gsheets at 200,365, below all five. Spatial is
eighth among core extensions at 2,399,569; it is not in this measured top five.

Sources: [core counts](https://extensions.duckdb.org/downloads-last-week.json),
[community counts](https://community-extensions.duckdb.org/downloads-last-week.json),
[metric definition](https://duckdb.org/community_extensions/download_metrics).

## HTTPFS: existing implementation, improve qualification and loading

`crates/httpfs` and the HTTPFS parity reports cover HTTP range reads, TLS/proxies,
S3 signing, listing/globs, uploads, multipart completion, region redirects,
version pinning and partitioned COPY/deletion. These reports include controlled
oracle comparisons; they do not by themselves prove real cloud interoperability.

There is an important UX difference from FTS/VSS: `crates/capi/src/extension_loader.rs`
recognizes a signed pinned HTTPFS package (commit prefix `4bc690d`, API v1.5.6)
and activates Rust code. It never executes that package's C++ machine code.
HTTPFS is not currently in `LINKED_EXTENSIONS`.

Proposed work: pin/review the complete upstream SQL surface, close actual gaps,
qualify against real S3-compatible services as well as protocol fixtures, and
move ordinary INSTALL/LOAD to the bundled offline flow already used by FTS/VSS.
Preserve explicit path/version behavior deliberately and regression-test it.
No new index or optimizer architecture should be necessary.
[Upstream surface](https://duckdb.org/docs/1.5/core_extensions/httpfs/overview).

## AWS: authentication on top of HTTPFS

S3 transport and static config secrets already exist. An `aws` secret type does
not mean the AWS extension exists: `create_s3_secret` in
`crates/roastedduck/src/secrets.rs` explicitly accepts only the config provider.

Implement the pinned credential-chain contract: environment and profile
precedence, process providers, STS/assumed roles, SSO, instance/container
credentials, web identities, validation, expiry and refresh. Check legacy
`load_aws_credentials` and any RDS support against the exact pinned source before
including them in scope. A Rust credential library may help, but its default
chain must not silently replace DuckDB's ordering/error/refresh semantics.
Audit transitive dependencies to avoid introducing C/C++ code.

Reuse HTTPFS request signing and scoped secrets; extend provider registration
and credential lifetime handling internally. Acceptance needs controlled
expiry/failure/precedence tests plus real temporary-credential integration.
[Upstream surface](https://duckdb.org/docs/1.5/core_extensions/aws).

## Azure: new remote filesystem

Azure secret-provider names and `az://`, `azure://`, `abfss://` routing are
catalog hints, not working implementations. Build Blob and ADLS path handling,
range reads, listing/globs, COPY writes and commit/failure semantics. Implement
the pinned connection-string, token, service-principal and credential-chain
authentication surfaces, including scope selection and token refresh.

The internal `FileSystem`/`FileHandle` traits in
`crates/function/src/registered_files.rs` provide the integration point; reuse
CSV/Parquet and memory accounting rather than introducing another query engine.
Rust HTTP/identity implementations can avoid the upstream C++ Azure SDK, but
their feature and dependency coverage must be audited. Acceptance requires
upstream SQL, protocol fixtures, an emulator where applicable, and real Blob/ADLS
checks; emulator success is not complete identity or ADLS evidence.
[Upstream surface](https://duckdb.org/docs/1.5/core_extensions/azure).

## ICU: maintain the existing Rust port

ICU is linked already. `crates/icu/src/icu4c` ports the relevant ICU 66 calendar,
timezone and collation behavior; the directory name does not mean it loads ICU
C++. Reuse this implementation. Recheck the pinned ICU SQL suite, collation keys,
Unicode normalization, DST boundaries and non-Gregorian calendars. Data updates
can change observable results, so upgrades need deliberate oracle comparison.
No new extension framework is indicated.
[Upstream surface](https://duckdb.org/docs/1.5/core_extensions/icu).

## PostgreSQL: largest architectural addition

Aliases and function/settings entries exist, but `DatabaseStorage` in
`crates/roastedduck/src/attached.rs` currently has only file and in-memory variants.
Full `ATTACH ... (TYPE postgres)` needs internal remote-catalog and transaction
interfaces, remote table binding, scans/pushdown, writes and DDL, connection
pooling, cancellation and error translation. A table function alone would not
satisfy that contract.

Use a Rust PostgreSQL protocol client instead of libpq, with audited TLS/auth
dependencies. Match libpq-style connection configuration where required, type
mapping (arrays, numeric, timestamps, enums and nulls), COPY transport and
snapshot consistency. A staged scan-only milestone is useful but must be named
as partial. Acceptance requires upstream SQL against actual PostgreSQL servers,
read/write and rollback tests, concurrent changes and failure/cancellation cases.
[Upstream surface](https://duckdb.org/docs/1.5/core_extensions/postgres/overview).

## Recommended delivery sequence and developer experience

1. Freeze each upstream extension commit associated with the 1.5.6 target and
   inventory SQL/settings/errors/tests. Versioned docs alone are not that pin.
2. Confirm ICU and HTTPFS gaps; make HTTPFS loading consistent with FTS/VSS.
3. Add AWS, which builds on existing S3 support.
4. Add Azure using the existing filesystem interface.
5. Add the remote catalog/transaction hooks and PostgreSQL implementation.

Keep these in the monorepo as separate implementation crates with internal Rust
interfaces. Ship implemented capabilities in normal packages, enabled by default;
use LOAD and offline INSTALL acknowledgement for bundled implementations.
Users supply credentials and endpoints, not C++ SDKs or extension binaries.
Preserve pinned autoload semantics. Run the oracle contract and conformance
ratchet after integration merges, then qualify installed Python/Homebrew packages.
Use the wording “roastedDuck's compatible implementation.” The stable C extension
bridge remains separate; no C++ compatibility layer is proposed.

Effort labels above are engineering judgments about scope, not delivery estimates.
No new implementation or cloud acceptance was performed in this assessment.
