# httpfs: DuckDB 1.5.6 contract

Implementation: roastedDuck's compatible implementation.

Upstream: https://github.com/duckdb/duckdb-httpfs/tree/4bc690dba4496c765777a0269d48fdbaff7cdc11

Pin is taken from DuckDB v1.5.6 `.github/config/extensions/httpfs.cmake`.
The adjacent `upstream.json` inventories 132 original SQL files and all
source hashes. Sources/archives are retained under `.context/artifacts/popular-extensions/`.

## Surface

HTTP(S) positional reads, S3-compatible read/write/list/glob/delete and multipart commit, Hugging Face access; all options, secrets and table functions registered by the pinned source. Preserve existing Rust transports. Ordinary INSTALL/LOAD becomes bundled and offline; explicit legacy artifact loading retains metadata/signature validation and never executes C++.

## Acceptance

Run unchanged upstream SQL against C++ DuckDB 1.5.6 and Rust. Keep missing-service
prerequisites explicit; skips do not qualify a feature. Add differential controls
for auth, scope, cancellation, transactions and errors where applicable. Use real
service integration in addition to protocol fixtures. Run scripts/conformance.sh
after integration merges, then verify normal Python and Homebrew packages.
No new C/C++ dependencies, binary adapters, default-off gates or public plugin ABI.

## Status

Contract recorded; not a claim that new functionality passes acceptance.
