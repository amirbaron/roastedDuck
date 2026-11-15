# azure: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-azure/tree/003214c96d0caa39d5c3e27a9e1976a0692c7d37

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/azure.cmake`.
The adjacent `upstream.json` inventories 27 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

Blob and ADLS filesystems for az://, azure://, abfss:// and abfs://; read/range/list/glob and COPY/write/commit; config, credential_chain, managed_identity, service_principal and access_token secret providers; all azure_* options registered in src/azure_extension.cpp. Match scope/authentication, retry/error and bounded-memory semantics.

## Acceptance

Run unchanged upstream SQL against C++ DuckDB 1.5.6 and Rust. Keep missing-service
prerequisites explicit; skips do not qualify a feature. Add differential controls
for auth, scope, cancellation, transactions and errors where applicable. Use local emulator/disposable-service
integration in addition to protocol fixtures, as authorized by the owner on
November 14. Record live-cloud behavior separately as unverified until exercised. Run scripts/conformance.sh
after integration merges, then verify normal Python and Homebrew packages.
No new C/C++ dependencies, binary adapters, default-off gates or public plugin ABI.

## Status

All 27 unchanged original SQL files pass locally on both engines. Blob, ADLS and
identity-provider implementations are present in normal builds. Broader core,
platform and release qualification remains pending; see the checkpoints below.

The pinned source additionally registers managed-identity client/object/resource IDs
and service-principal certificate authentication. These are required even though
the engine's generated provider-name hints omit managed_identity. Source inventory
takes precedence over that hint table.

## Blob implementation checkpoint (November 15)

Normal builds now provide bundled offline INSTALL/LOAD azure, secret type/provider
binding and a Blob filesystem. The implementation reuses the existing Rust HTTP
transport without loading HTTPFS as an extension or adding native dependencies.
It implements scoped and fully qualified paths, SharedKey/SAS authentication,
anonymous/access-token request construction, range reads, paginated listing and
globbing, deletion, bounded block uploads, intermediate commits, and separate Azure
HTTP statistics. HTTP and filesystem logging include configurable header/query
redaction. Provider metadata covers all five upstream providers; credential-chain,
managed-identity and service-principal token acquisition remains unimplemented.

Eight unchanged original files pass on C++1.5.6 and Rust against fresh Azurite
storage per engine: `azure_secret`, `azure_scope_and_full_path`, `azure_glob`,
`azure_writes`, `azure_writes_more.test_slow`, `fs_logs`, `http`, and
`http_log_redaction`. The larger write file exercises 1,000 small objects, concurrent
writes, large CSV and partitioned Parquet. Original SQL and data are unchanged.

`azure.test` retains a raw failure at line 51 on both engines: its expected profile
says three HEADs and zero GETs. C++ actually reports three HEADs/two GETs; Rust
reports two HEADs/one GET. The actual plans and counts are retained; no extra
requests are fabricated and the original expectation is not changed. Azure stats
count response Content-Length, including HEAD, separately from HTTPFS's streamed
byte counters. This common original failure does not imply identical profiles.

Twelve paired controls pass for escaped/Unicode names, SharedKey and SAS reads,
wrong-key rejection, all provider metadata, invalid parameters and actual SAS-log
redaction. These metadata controls do not establish identity-provider authentication.
The existing signed S3/process-refresh differential still passes all four cases,
with 17 requests and zero invalid signatures after the shared transport changes.
Strict all-target Clippy passes for function/HTTPFS/runtime/SLT.

Receipts under `.context/artifacts/popular-extensions/`:
`azure-upstream-blob-3/`, `azure-blob-control-2/`, `aws-after-azure-control-1/`,
and `azure-clippy-final.log`. These are signed development-image checks, not
release-image or cross-platform qualification. Reproduction scripts are documented
in `scripts/azure-tests/README.md`.

Still required: token providers, ADLS and its protocol fixtures, remaining originals,
complete context-cache/read-transfer semantics, cancellation and failure controls,
full conformance, and package/release gates. Azurite is not ADLS or live Azure.

Identity reference sources are retained with SHA-256 hashes under
`.context/artifacts/popular-extensions/azure-sdk-reference/`. The pinned vcpkg
baseline selects [azure-identity-cpp 1.13.2](https://raw.githubusercontent.com/microsoft/vcpkg/84bab45d415d22042bd0b9081aea57f362da3f35/ports/azure-identity-cpp/vcpkg.json)
and [azure-storage-blobs-cpp 12.15.0](https://raw.githubusercontent.com/microsoft/vcpkg/84bab45d415d22042bd0b9081aea57f362da3f35/ports/azure-storage-blobs-cpp/vcpkg.json).
Use those source contracts for the remaining providers; do not substitute an
evolving SDK default chain.

## ADLS protocol checkpoint (November 15)

Normal Azure loading now also registers the Rust ADLS filesystem for `abfss://`
and `abfs://`, including remote-path access checks and autoload hints. ADLS uses
directory-aware metadata/listing, recursive traversal with the pinned glob
restriction, file/directory creation, deletion, append and flush/close operations.
Blob and ADLS share bounded write buffers but use their distinct commit protocols.
The no-secret ADLS path rules deliberately differ from Blob's legacy settings,
matching the pinned implementation.

The independent REST fixture rejects unsigned requests, checks every SharedKey
signature, and retains append/flush traces. C++ and Rust each pass all 12 SQL
controls across both schemes, with six close=true flushes each, including
intermediate commits and partitioned Parquet. The C++ fixture run is separately
retained. Nine unchanged Blob/VFS originals now pass on both engines, including
`azure_vfs_ops.test`; the previous eight remain passing. No original was edited.

Receipts: `azure-dfs-control-1/`, `azure-dfs-oracle-2/`,
`azure-dfs-blob-regression-1/` and `azure-dfs-clippy-final.log` under
`.context/artifacts/popular-extensions/`. The first fixture attempt lacked two
required Blob property headers and incorrectly framed HEAD errors; its failed
oracle receipt remains. Corrections follow the pinned SDK REST response parser.
Adding serde_json to the transport crate reuses its existing locked version;
no new package or native dependency is introduced.

This is partial Azure acceptance. Identity token acquisition, remaining cloud
originals, complete option/cancellation semantics, full conformance and release
qualification remain required. The profile original's raw failure above remains.

## Identity acquisition checkpoint (November 15)

Rust now resolves credential_chain, managed_identity and service_principal tokens
for Blob and ADLS. The implementation follows the pinned default/explicit provider
order, AZURE_TOKEN_CREDENTIALS selection, environment/workload settings, CLI command
and environment, managed-identity endpoint precedence and selectors, token lifetime
parsing and proactive refresh rules. RSA certificate assertions use ring; no new
crypto/native package is added. Unix CLI subprocess groups are owned until exit and
are killed/reaped on interruption. The SDK's 13-second CLI timeout is preserved.

Twenty-five paired local identity controls pass, with matching provider request
traces and CLI arguments/environment for all cases. These include direct/environment
client secrets, certificates (independently verified RSA signatures and x5t/kid
thumbprints), workload tokens, CLI/default fallback, managed App Service 2019/2017,
Cloud Shell, IMDS, client/object/resource selectors, malformed configurations,
untrusted managed TLS, legacy timestamp cache behavior and nonzero CLI exit with
valid token JSON. The pinned direct-managed provider drops custom transport options;
the fixture and Rust preserve that behavior rather than broadening its CA trust.
Azure Arc's restricted challenge-file handling is implemented; only its pinned
macOS unsupported-platform result has been exercised, not Linux/Windows Arc success.

The actual Rust public API returns ErrorKind::Interrupt while the fake CLI is
running; both API and native CLI controls prove the helper and descendant stop.
The final signed development image retains nine passing Blob/VFS originals and
12 passing ADLS protocol controls per engine. Strict all-target HTTPFS/runtime/SLT
Clippy passes. libc becomes a direct dependency using its existing locked version;
no package versions or native dependency set change.

Receipts under `.context/artifacts/popular-extensions/`:
`azure-identity-control-4/`, `azure-interrupt-api-final/`,
`azure-interrupt-cli-final/`, `azure-identity-blob-regression-1/`,
`azure-identity-dfs-regression-1/`, and `azure-identity-clippy-final-2.log`.
Earlier failed fixtures are retained: the first CLI fixture incorrectly expected
an environment variable that C++ strips, and managed HTTPS incorrectly depended on
custom CA propagation; the first interrupt CLI input lacked its terminating newline.

Still required: unchanged cloud original suites through local protocol fixtures,
remaining option/refresh/challenge/error and network-cancellation controls,
Linux/Windows Arc acceptance where applicable, full conformance, cross-platform
package qualification and publication. These checks do not establish live Entra,
Azure policy or production-cloud acceptance.

## Complete original SQL cohort (November 15)

`azure-upstream-all-2/` records **27 PASS, zero FAIL/SKIP** on both C++1.5.6 and
Rust, with zero fixture errors and zero invalid authorizations. This includes
all identity, proxy, anonymous-read, hierarchy, integrity and large/concurrent-write
originals. SQL and data were copied without edits and checked against their pins.
The full cloud fixture routes only owned account hostnames through a loopback
CONNECT proxy, with CA trust scoped to the child processes. Persistent secrets
are real files in an owned directory, not substituted SQL results.

The initial full run exposed a Rust Parquet integration gap: nonlocal row groups
were not using the normal prefetch/external-cache path, and scans reused the bind
handle. Remote scans now revalidate through a separate handle and bypass provider
read-ahead when the engine cache owns the requested range, matching InitializeScan.
Local-file defaults and the external-cache budget are unchanged. Azure's engine
cache validation uses Last-Modified, as upstream does; ETags protect chunks within
each download, rather than changing the engine's cache-validation contract.
This resolves the profiling original without fabricating request counts.

The earlier fresh-Azurite profiling failures above are retained but do not describe
the final cohort. The pinned engine deliberately invalidates timestamp-only cached
data less than ten seconds old. The final fixture starts with the original data's
timestamps and updates metadata on writes. The fixture also implements the C++
transport's 100-continue upload handshake and performs concurrent TLS handshakes in
workers. Its normal listing size follows the [Azure REST contract](https://learn.microsoft.com/rest/api/storageservices/datalakestoragegen2/path/list).
Dedicated small-page controls still verify continuation handling.

Fourteen transfer controls per engine pass on the same source in
`azure-transfer-control-final/`: exact bytes, configured chunk limits, bounded
parallelism, zero/negative concurrency and ETag overwrite races. The configured
read buffer is now used, with bypass for larger/direct reads; SDK-style chunks
reuse the first response's ETag and stop scheduling after a failure. The failed
buffer is discarded. ADLS metadata/downloads use the Blob endpoint while path
mutation and listing use DFS, following the pinned SDK.

This closes the original-SQL milestone, not the full five-extension goal or release
qualification. Core regression/conformance, remaining provider/error/network
controls, applicable platform checks and package/publication gates remain required.

Core component follow-up: signed function (37) and Parquet (31) unit tests pass;
two existing Snappy comparison/benchmark tools remain ignored. Strict all-target
Clippy passes for function, HTTPFS, Parquet, runtime and SLT. Receipts:
`azure-core-tests-final/` and `azure-core-clippy-final.log`. Full core conformance
and release-image qualification are not implied by these component results.
