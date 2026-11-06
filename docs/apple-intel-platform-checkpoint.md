# Apple Intel platform checkpoint

The iOS Intel simulator engine and macOS Intel core/shell/SQL runner now build from
source32 with full release LTO and one Cargo job. Artifact validation checks their
actual architecture, Mach-O platform/deployment metadata and public exports before
freezing. The copied dylibs use `@rpath/libduckdb.dylib`; native signatures and signed
content manifests verify with the existing local identity and trusted certificate pin.
The shipping libraries expose547 DuckDB functions and no opt-in ADBC test bridges.

## iOS Intel simulator

The frozen slice is x86_64, iOS Simulator platform7, minimum15.0, SDK27.1. Dylib:
`a4dbd1378dd2d33579c83ae79c2a1e3b4cedd77c4536abbb3b87ae9cf7fd9db8`;
static archive:
`209ee7f895f21575c3eaaa68d32d53c3f424b68ee6ebe31cd6e63c1cc9511a55`.

This is compile/sign proof only. The installed iOS26.5 runtime executes arm64 only.
The separate arm64 simulator runtime also remains pending CoreSimulator access to
the dedicated external-SSD device set; no internal-storage fallback is used.

## macOS Intel

All three build stages actually exit zero before freezing: native library/archive/
CLI/SLT, four precision regressions and three public instance-cache regressions.
Mach-O metadata identifies x86_64/macOS platform1, minimum11.0. Signed identities:

| Artifact | SHA256 |
|---|---|
| Native dylib |`4bfbdd0b340c08109ffdb4fd55dd2d481869b9fe75808944a126d0aa1bd308b0`|
| Static archive |`2050ed174ab59865896f1746376139fa0022d0c30171a219bc5188e16eef3369`|
| Shell |`172d4af5bc8f54072d15f78f0d6b14f2d92ec2fa266fe305d4869d11cfdceafc`|
| SQL runner |`78b0338d0987aafc72c4d851adf037cadb7a6ff36e8110c3d6079f5fc4bb7558`|

Under explicit `arch -x86_64` Rosetta execution on the Apple arm64 host, all four
integer-average regressions and all three cache regressions pass. A separately
launched shell returns the expected UTF-8/scalar/extended-precision average row.
CSV is parsed for those literal values; byte-for-byte shell parity is a separate
remaining gate. The signed payload manifest verifies before and after execution.

These are Rosetta correctness checks, not Intel-hardware performance measurements.
Full paired SQL, shell, C API/ADBC, Python/Go/Swift, persistence/WAL and performance
acceptance have not yet been established for this target.

## Frozen C++ comparator

The C++ Intel comparator configuration completes for the supplied1.5.6 archive,
with an explicit x86_64/Darwin target and `osx_amd64` platform metadata. The synthetic
Git identifier0123456789 is recorded as such. All seven standard/tested extensions
are configured: JSON, ICU, TPC-H, TPC-DS, autocomplete, core functions and Parquet.
The full serial CMake ALL build completes, including the shell, unit tests and
loadable test assets, followed by the oracle shim. The frozen comparator's native
images are signed; a separate signed manifest authenticates all16 loadable modules
without changing DuckDB's appended metadata. Its public export set matches Rust's
547 functions exactly. The oracle resolves the frozen comparator dylib. The C++
shell and unit executable contain their own statically linked engine, which is
recorded explicitly rather than requiring a nonexistent dylib dependency.

Both engines subsequently pass all30 C API files/155 cases under Rosetta. The
connection warning and terminal-buffering corrections then pass all46 PTYs and
batch byte controls. The complete shell run retains four build-source metadata
differences, one existing stack-frame exception and five identical upstream
Rosetta failures. See [the exact shell outcomes](rosetta-shell-parity.md).
The complete canonical4,870-file SQL pair finishes on2026-10-30. C++ records
4,796 passes,72 skips,one300-second gzip timeout and one concurrent-metadata
INTERNAL failure. Rust records4,783 passes,72 identical skips and exactly the15
previously documented exceptions, with no crashes/timeouts or unexpected failures.
All17 verdict differences are those15 exceptions plus Rust passes for the two
reference nonpasses. Every skip reason matches. All4,870 verdicts per engine are
present exactly once;1,522 data fixtures, SQL inputs and all three signed artifact
groups verify afterward. The unchanged4,855-file ratchet passes with zero regressions.
This establishes the frozen Intel candidate's full SQL gate under Rosetta, not
other platforms, native hardware performance or final port acceptance.

The72 reference skips include43 requiring HTTPFS; other reasons are preserved in
`intel-cpp-canonical-complete-1.json`. Neither skipped configured-extension tests
nor the seven linked extensions establish the broader optional extension surface.
The gzip timeout remains a reference resource-bound followup. The metadata fault
reproduces in the unchanged upstream C++unit runner and a separate oracle control,
independently of the Rust adapter; see the [reference limitation](../conformance/divergences.md).
All original outcomes remain preserved.

During the Rust half, parallel batch-insert space reclamation
(`test/sql/storage/parallel/reclaim_space_batch_insert.test_slow`) passes on both
engines but takes108.611s versus C++33.631s, a3.23x whole-file timing outlier.
`intel-reclaim-batch-timing-outlier-1.json` retains the results and input/log hashes.
These are diagnostic Rosetta timings without paired peak RSS, not native hardware
performance acceptance or proof of a regression against an older source. Query/phase
timing and memory followup remain open; no cache or memory-budget increase is approved.

## Evidence

Dedicated SSD root: `.context/artifacts/platform-matrix-20261029/`.

- `source-32/`: guarded workspace snapshot; product code unchanged from the
  corresponding accepted component sources.
- `apple-x86_64-apple-ios-build-4/`, `iossim-x86_64-native-freeze-1.json` and
  `work/apple/iossim-x86_64-native-signed-1/`: actual build and signed simulator slice.
- `macos-intel-native-build-1/`: all successful parent/stage exits and Cargo JSON.
- `macos-intel-native-freeze-1.json`, `work/apple/macos-intel-native-signed-1/`:
  signed native images and regression executables.
- `work/apple/macos-intel-sdk-run-1/verification.json`: seven tests, CLI values and
  signatures before/after, labeled Rosetta execution.
- `cpp-macos-intel-build-1/`: frozen archive hash, exact configuration, serial build
  and completed oracle stage.
- `intel-cpp-canonical-complete-1.json` and
  `work/apple/paired-corpus-intel-1/`: completed reference results, exact skip
  reasons, both complete results, original failures and post-run fixture/signature checks.
- `intel-corpus-conformance-verification-1.json`, `intel-corpus-ratchet-check-1.json`
  and `intel-corpus-dashboard-1.md`: full reconciliation and unchanged ratchet.
- `intel-metadata-reference-classification-1.json` and
  `work/apple/intel-metadata-followup-1/`: independent reference-fault reproduction.

Final integrated release acceptance, the remaining platforms and Python ABI
versions remain open. The accepted lower-memory policy and individual outlier
reporting requirements are unchanged.
