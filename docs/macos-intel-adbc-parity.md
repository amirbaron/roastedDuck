# macOS Intel ADBC checkpoint

On 2026-10-30 both frozen engines pass the full native ADBC suite: 64 cases and
3,040 assertions each. An independent public-ABI consumer also verifies prepared
Arrow results, rollback, commit, CHECKPOINT and file reopen against each
feature-free shipping library. Execution is Intel under Rosetta on an arm64 Mac;
this does not establish other platforms or native performance acceptance.

The upstream suite's private native helpers use a separate signed Rust
`adbc-test-api` bridge with exactly three extra exports. The immutable shipping
library remains `4f826321a230eee4312a175150c990051209ed30c72781e0268ec72cac130104`,
with 547 DuckDB exports and no helper hooks. The mutable Cargo native build is
restored and checked for 547 exports/no hooks afterward.

`scripts/adbc-tests/run.sh --build-only` permits compiling and native-signing
the harness before its executable content manifest is signed and verified.
Both harnesses verify the selected engine before/after execution. Executables,
libraries and fixtures authenticate before/after, and upstream assertions remain
unchanged. The public consumer calls only the published driver function table and
Arrow interfaces; it uses no private bridge or C++ engine types.

The first full attempt records 62 passes/two failures on both engines: the fresh
working directories lacked the unchanged relative CSV fixtures for lineitem and
flights. Their original failures remain preserved. A fresh working directory
links the frozen reference data and reuses the same signed executable; all 64 cases
then pass. Both CSV hashes remain exact, with no test filter or assertion change.

Evidence under `.context/artifacts/platform-matrix-20261029/`:

- `adbc-intel-verification-1.json`: reconciled native/public/default-build gates.
- `work/apple/adbc-intel-rust-bridge-{build,signed}-1/`: separate test bridge.
- `work/apple/adbc-intel-full-{1,2}/`: original setup failures and successful full runs.
- `work/apple/adbc-intel-default-restore-1/`: feature-free build restoration.
- `work/apple/adbc-intel-public-consumer-1/`: signed public consumer and both runs.

Final common-source release artifacts, other targets, packaged clients and coupled
speed/memory acceptance remain separate requirements. No engine cache or memory
budget was increased for these checks.
