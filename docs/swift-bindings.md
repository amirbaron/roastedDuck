# Swift client parity

Completion scope includes Swift alongside all primary clients and C#/.NET/Julia.
Use the wrapper and tests bundled in frozen DuckDB1.5.6 under
`tools/swift/duckdb-swift`, not an unpinned newer package. Preserve its MIT license.

## Current source readiness audit

There are28Swift source files,9test files and132test methods, with182public declaration
lines in the static inventory. These counts are source observations, not test/runtime
acceptance. The105C API functions invoked by the wrapper all have exports in signed
Rust library2;12other duckdb_*invocations are imported C struct/enum constructors/type
casts present in the frozen header. No missing function symbol is inferred from those.
The exact source/header hashes and method/call inventory are in
`.context/artifacts/client-readiness-20261025/swift-source-inventory-1.json`.

The original Package.swift.template creates a Cduckdb target by bundling C++engine
sources. Replace that package wiring with a native header/module and linkage to the
Rust library, while retaining wrapper/test code and exact engine identity evidence.
Do not run the upstream generator to accidentally compile the C++engine. The current
Rust dylib's absolute install name also needs a portable distribution solution; test
link pinning alone is not package acceptance.

A Swift executable is available on this Mac. The source audit did not execute compiler,
build or test workloads because the full SQL corpus is active. After it terminates,
run the complete selected upstream suite and actual installation/ownership/type/
transaction/file/WAL journeys, sign native artifacts before use and verify the loaded
Rust library with no C++fallback. CPPcontrols must use the same pinned wrapper/tests.
Keep source and small tooling in the workspace; copies/builds/caches/module cache/test
output on the dedicated SSD. Serialize builds/tests/benchmarks with one job/nice10.
Other clients and whole-port performance/storage acceptance remain open.

## Preparation contract

`swift/upstream.json` pins the bundled source/test/license and C header hashes.
Prepare separate fresh test packages for each explicitly selected signed library,
copying wrapper/tests byte-for-byte. A minimal C target includes only the frozen C
header; no C++engine source generator runs. Record source/library/manifest hashes and
link directories. Test-specific linker paths/header padding permit pinning the built
executables to their immutable engine image before signing and running.

This prepared package is test wiring, not completed portable distribution. It must
not overwrite an earlier output, silently select a system library, alter assertions,
run a build during preparation, or claim that the132tests passed from inventory alone.
Build/cache/scratch/temp paths and actual loaded-library verification follow after the
active corpus terminates. Validate Foundation cache/temp placement as well as TMPDIR.

## Prepared controls (2026-10-25)

`scripts/swift-tests/prepare.py` prepares separate explicit Rust/CPP packages without
building or running tests. Both generated packages preserve all39pinned wrapper/test/
license files byte-for-byte, include only a minimal C header target and contain no
C++engine sources. The selected native image is signature-verified before preparation;
its path/hash and generated manifest hash are recorded. Existing outputs are refused.

Prepared controls and comparison are under
`.context/artifacts/swift-client-20261025/{rust-package-1,cpp-package-1,preparation-comparison-1.json}`.
The generated absolute linker paths are test setup, not portable distribution acceptance.
A toolchain help/version probe reports Apple Swift6.4 targeting arm64macOS27.0; full
help output is retained under `toolchain/`. This did not compile the package or run
its132test methods. The full core corpus remains the only heavy workload.
