# Linux arm64 ADBC checkpoint — 2026-10-30

The full frozen native ADBC suite passes **64 cases /3,040 assertions on each
engine**,without test filters. A separate public-ABI consumer passes prepared Arrow
queries,rollback/commit,CHECKPOINT and file reopen against the feature-free signed
Rust shipping library and frozen C++ library. This is a Linux arm64 checkpoint,
not whole-release,other-platform or performance acceptance.

## Native suite and shipping boundary

The upstream suite contains private native-connection helpers. Its Rust run uses a
separate opt-in `adbc-test-api` library with exactly three helper exports:
connection,database and error initialization. That bridge is frozen and signed
before execution. It is not shipped. The normal build target is restored afterward
and checked for547 DuckDB exports with no bridge hooks.

The immutable shipping library remains unchanged:
`6066d376cd6c75794bb16a0be61a791b1b6d3688752c636213e61414445e1327`.
The full bridge run records its actual loaded library through Linux loader
diagnostics and verifies signatures before/after. The frozen C++ control also
passes all64/3,040 with signed integrity checks.

The public consumer uses only the published ADBC driver function table and Arrow C
Data/Stream interfaces. It loads the selected library directly,records `dladdr`,
creates a persistent table,rolls back an insertion,commits another,prepares and
streams the independent expected rows1/2/3,checkpoints,closes and reopens the file,
then repeats the Arrow result check. It has no private bridge or fake engine types.
Its executable and both selected libraries are authenticated before/after runs.

## Retained setup failures

The first C++ suite passes63 cases but fails the relative-path URI case because
the harness runs in a read-only source tree. An optional dedicated working directory
with an unchanged reference `data/` link permits relative database writes. A fresh
full suite then passes64/3,040. No assertion or source test is removed.

Linux test-only C++ stand-ins and the native reference bridge compile with hidden
visibility,preventing helper symbols from preempting the selected engine. This
follows the independently diagnosed C API harness interposition issue; engine
code and shipping ABI are unchanged.

The first standalone public-consumer compile uses C mode,but the frozen DuckDB ADBC
header includes C++ Arrow helper methods. The retained second build uses C++ for
that header while calling only the public C ABI. Both actual public-library runs
pass. This is a test consumer adaptation,not C++ code in the Rust engine.

## Evidence

Root: `.context/artifacts/platform-matrix-20261029/`,on the dedicated external SSD.

- `work/linux-arm64/adbc-cpp-2/verification.json`: full C++64/3,040 gate.
- `adbc-rust-bridge-build-1/`, `adbc-rust-bridge-freeze-1.json` and
  `adbc-bridge-hooks-1.json`: separate signed bridge and exact hook audit.
- `adbc-restore-default-1/terminal.json`: normal target restored,547 exports/no hooks.
- `work/linux-arm64/adbc-rust-1/verification.json`: full Rust64/3,040,actual image
  and signature evidence.
- `adbc-public-freeze-2.json` and `adbc-public-run-1/verification.json`:
  feature-free public consumer and both selected-library runs.
- `source-14/manifest.json`: harness-only working-directory/visibility adaptations;
  production engine inputs remain those of the integer-average candidate.

Python's installed ADBC facade,current Python/Swift clients,other OS/architecture
targets,full final core/shell/storage/WAL and coupled performance gates remain open.
No memory/cache increase or performance acceptance is inferred from these tests.
