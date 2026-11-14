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

In progress. Blob storage has a first Rust implementation; full Azure compatibility
is not yet accepted. See the verification checkpoint below.

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
