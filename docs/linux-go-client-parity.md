# Linux arm64 Go client checkpoint — 2026-10-30

The pinned Go client passes **652 root-suite cases on each engine**,including Arrow,
with identical case IDs/verdicts. A separate module also passes **four cross-engine
checkpoint/WAL journeys** against the current signed Rust library. This checkpoint
does not establish other architectures,all client/platform acceptance or performance.

The client is `github.com/duckdb/duckdb-go/v2` v2.10506.0,commit
`addeaeb4ad0a9040c4f8dd70757e17e3852d5881`,with Go1.27.3 on Linux arm64.
All79 source files and the bindings header match the repository's frozen pin.
Toolchain,module/build caches,temporary files,binaries and evidence are on the
dedicated external SSD. Builds and tests run sequentially; runtime swap is disabled.

## Root suite and selected engines

Signed Go executables authenticate their selected libraries and record both ELF
dependency resolution and actual runtime library initialization. No selected tests
are unfinished or unstarted. Inputs and binary hashes remain stable before/after.

| Engine | Go cases | Selected library SHA256 |
|---|---:|---|
| Frozen C++1.5.6 |652 pass| `7a5bbd5e0319aede9ceee37b604ff4953297c08aee276931d1c5e64623260d0c` |
| Current Rust precision candidate |652 pass| `6066d376cd6c75794bb16a0be61a791b1b6d3688752c636213e61414445e1327` |

The Go build/test tools now support native Linux and macOS architectures,explicit
manifest certificate trust and selected-library checks. An initial Linux loader
check assumed SONAME-to-path output; Rust's absolute `DT_NEEDED` path is also valid.
Both loader forms pass,and selecting a different library is rejected. The original
failed receipt is retained; the successful Rust build uses a fresh output path.

## Separate consumer and persistence

The consumer builds as its own Go module around the pinned upstream client. It
checks prepared insertion,Appender writes,NULLs,DECIMAL,TIMESTAMP,BLOB,list and
struct values,duplicate-key rejection and rollback. Nested fields are projected
and checked independently; raw textual displays are retained separately.

Each journey snapshots the database and any WAL before replay. The receiving engine
reads the typed rows,performs a mutation and CHECKPOINT; the original engine then
reads the mutated result. Actual `dladdr` library identity and signatures are
checked throughout.

| Writer → receiver | Mode | Result | Nonempty WAL before replay |
|---|---|---|---:|
| C++ → Rust |CHECKPOINT|pass|not required|
| C++ → Rust |WAL|pass|440 bytes|
| Rust → C++ |CHECKPOINT|pass|not required|
| Rust → C++ |WAL|pass|429 bytes|

The first orchestration attempt had a generated Python syntax error before consumer
compilation. Its failure is retained. The first journey expected quoted STRUCT
string display; C++'s actual display omitted those quotes. That mismatch is retained,
and the next source adds independent nested-field assertions before fresh signed
consumers and fresh journeys. No engine code or upstream test assertion is changed.

## Evidence and remaining gates

Root: `.context/artifacts/platform-matrix-20261029/`,on the dedicated SSD.

- `average-go-verification-1.json`: exact652-case reconciliation and loader paths.
- `work/linux-arm64/average-go-cpp-{build,test}-1/` and
  `work/linux-arm64/average-go-rust-{build,test}-2/`: build/runtime reports and logs.
- `average-go-{cpp,rust}-freeze-*.json`: trusted signed executable manifests.
- `go-loader-forms-1/`: SONAME/absolute path checks and wrong-library rejection.
- `go-consumer-preparation-{1,2}.json`: original source and recorded adaptations.
- `work/linux-arm64/go-consumer-{cpp,rust}-build-3/`: final signed consumers.
- `go-consumer-freeze-2.json` and `work/linux-arm64/go-consumer-journeys-2/`:
  four successful journeys,all raw reports and pre-replay snapshots.

Linux x86_64 and final updated Mac client gates remain open. Broader core,Python,
Swift,ADBC,shell,storage/WAL and coupled performance acceptance remain separate.
The accepted lower-memory policy is unchanged; this functional checkpoint makes no
speed/RSS acceptance claim and introduces no caching to close C++ speed gaps.
