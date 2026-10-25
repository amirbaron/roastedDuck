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

## Serialized build and signing contract

Prepare a build-only runner using the observed Swift6.4CLI flags: one job, explicit
package/scratch/shared-cache/config/security paths, with temporary and module caches
on the dedicated SSD. It must refuse a live SQL corpus, verify pinned source/header/
manifest/library hashes, retain complete build logs and preserve prior outputs.

After a successful build, inspect project Mach-O executables/dylibs/bundles, pin any
libduckdb load command to the selected immutable image, then sign and strictly verify
those artifacts and test bundles before execution. Recheck library/source hashes.
This step stops before tests. A later Foundation cache/temp probe must prove that its
user-cache-derived test directories are on dedicated storage; TMPDIRalone is not proof.
Only then run the complete upstream suite and actual loaded-library/ownership/data
journeys. Portable distribution and other platforms remain independent open gates.

## Build-only runner prepared

`scripts/swift-tests/build.py` implements the serialized build/sign step and stops
before test execution. Its live-corpus refusal was exercised: exit2with no build output
directory or compiler/test launch. The guard evidence is retained under the Swift
artifact task's `toolchain/live-corpus-guard-1.*`. No Swift compilation has run yet.

The runner preserves initial command/input metadata and build exit status, pins actual
project Mach-O dependencies before signing, signs test bundles, then records final
artifact hashes after bundle signing. Runtime library identity, full upstream tests,
Foundation cache/temp placement and portable distribution are still unverified gates.

## Foundation storage precondition

The upstream database test helper requests an item-replacement directory appropriate
for the user caches directory. Prepare a standalone Foundation probe that reports
home/cache/temp/replacement paths under an explicit dedicated test root. It must stop
before writing probe data if base or returned paths escape that root. A small marker
is written/read only after the replacement location passes the containment check.

Compile/sign/run this probe only after the corpus terminates, with test-only
CFFIXED_USER_HOME and TMPDIR settings and external module caches. Preserve its actual
path report and status; environment variables alone do not prove placement. A failed
precondition must gate the data-writing Swift suite. Any later fixture-path adaptation
must preserve upstream assertions and be applied equally to CPP/Rust controls.

## First Foundation storage observation

The signed probe's user home/cache redirect is honored, but generic temporaryDirectory
still points at the internal Darwin user temp directory despite an existing external
TMPDIR. The first probe exits2before marker writes; raw paths/status/source/signature
hashes remain under `storage-probe-1/`. Collect environment/directory diagnostics and
the actual item-replacement path before selecting a fixture adaptation. This failure
keeps the data-writing Swift suite gated; it is not client/runtime acceptance.

## Measured fixture path adaptation

Probe2 confirms TMPDIRexists/is writable but Foundation ignores it for generic temp;
itemReplacementDirectory chooses the SSDvolume's shared .TemporaryItems directory,
outside the dedicated workspace root. It exits2without marker/database writes.
Source/signature/path reports remain under storage-probe-2. The core corpus is now
terminal and accepted; these probes are serialized after it.

The owner's explicit dedicated-storage requirement takes precedence over keeping a
path helper byte-identical. Adapt only DatabaseTests.generateTemporaryFileURL in both
CPP/Rust copies to RUSTDB_SWIFT_TEST_DIRECTORY plus a unique child directory. Preserve
all assertions, record the exact original/patched hashes and path-only diff, retain
original prepared controls, and require the environment at runtime. Update input pin
verification to recognize only that recorded adaptation. Test functionality is not
weakened; actual database files still use the upstream API/cases.

Verify the adapted fixture behavior with the signed storage probe's explicit mode.
Report original Foundation paths separately; success of explicit fixtures must not
be relabeled as proof that Foundation's generic temp/replacement redirect worked.

## First control build linkage correction

The first CPPcontrol compiles the wrapper/tests but fails native symbol linkage.
Its generated Products/Debug contains libDuckDB.a; -lduckdb resolves that similarly
named wrapper archive ahead of the selected native directory on this case-insensitive
volume. The selected arm64CPPlibrary exports duckdb_open and the required APIs.
Preserve cpp-build-1logs/exit status; this is package wiring, not Rust engine evidence.

Prepare fresh controls with the explicit native library file passed to the linker,
retaining RPATH/header padding and subsequent install-name pinning/signing. Avoid
search-name ambiguity rather than changing wrapper/test behavior or engine code.

## First complete CPP control execution

Explicit file linkage fixes the control: cpp-build-2completes, its native test bundle
is pinned/signed/strictly verified, and all132upstream XCTestcases pass. SwiftTesting
also emits a separate zero-test suite; it is not used as evidence for the132cases.
Per-case start/pass IDs and all raw logs are retained. Native artifact/library hashes
remain unchanged after execution.

DYLD_PRINT_LIBRARIESdoes not survive this Swift launcher path, so no loaded engine
path is present in the control log. Do not infer runtime image proof from that absence
or linker commands alone. Add one separately identified in-process identity test to
CPP/Rust packages that records dladdr/the loaded libduckdb image and compares it with
the explicit expected path. Keep132upstream assertions unchanged and report this
additional identity test separately. The firstRust build is in progress; it has not
yet established runtime client parity.

## Test execution and identity accounting

Run tests from an already signed/pinned build with --skip-build/--no-parallel and
explicit external cache/config/security/scratch paths. Before data tests, rerun the
signed dedicated fixture probe for that run's root. Require the expected native
library path and a new identity-report path in the additional test's environment.

Parse all XCTestcase start/pass/failure IDs; keep the132upstream cases separate from
the one identity case. SwiftTesting's separate zero-case report cannot replace that
accounting. Verify the in-process function/image report and unchanged source/library/
artifact hashes/signatures after execution. Missing identity or case coverage is a
failed verification even if the launcher exits0. Retain complete logs/exit statuses.

## Current-host complete upstream suite checkpoint

Both CPPandRust controls pass all132upstream XCTestcases plus one separately identified
in-process identity test, with exactly the same133case IDs. Rust loads only the selected
signed library2 (SHAb958e90af7ee0b154d10163c8949191a3ee9fee4d190232b55ebbf5ef58b6c4c);
CPPloads only its frozen reference. dladdr and the process image list verify that inside
the test process, despite launcher environment tracing being stripped. Source/library/
project artifact hashes/signatures remain unchanged after execution. CPP's source-tar
version string v0.0.1andRust'sv1.5.6remain recorded raw, not normalized.

Tests/assertions are identical across engines. The one upstream test-helper path
adaptation redirects database fixtures to a dedicated directory; all assertion code is
unchanged. The signed explicit storage probe confirms cache/fixture containment and
marker readback, while reporting the failed generic Foundation temp redirect separately.
The first search-name linkage failure, missing loader trace and original path probes
remain preserved. SwiftTesting's independent zero-test suite is not the132-case proof.

Evidence: `.context/artifacts/swift-client-20261025/verification-1.json`,
`{cpp-test-2,rust-test-1}` raw logs/case IDs/identity reports, signed build inventories,
prepared source hashes and `tooling-source-verified-1/`. This is current macOSarm64/
Swift6.4functionality evidence. The absolute library-file test linkage is not portable
SDK packaging. Complete public API coverage, independent client journeys, other platforms
and broader file/WAL/ownership/performance acceptance remain open.
