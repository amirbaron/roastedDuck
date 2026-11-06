# Apple Swift package verification

Current required release evidence (November 4, 2026) uses source2253, Mac2268 and iOS2301, packaged as `work/apple/swift-arm64-current-package-2302`. The three required ARM64 slices have matching547 public C API exports and zero private test hooks. Current Mac packaged consumers and all133 simulator Swift cases, a typed consumer and four cross-engine persistence/WAL journeys passed scoped review2314/2388. Physical iOS execution and provisioning remain unavailable and unverified; simulator results do not establish device execution. Final general storage/performance/artifact acceptance remains open. Historical Intel and older-source receipts below retain their original scope. See [current verification](current-source-verification.md) and [known exceptions](release-known-exceptions.md).

The owner removed required Mac Intel support on 2026-10-30. The required release
plan is macOS arm64, iOS arm64 devices and iOS arm64 simulators. The already tested
Intel Mac/simulator artifacts remain historical or optional compatibility evidence.
`scripts/swift-tests/package-apple.py` now requires only the three arm64 slices and
accepts optional Intel inputs. The existing `package.py` is the earlier Mac arm64 harness.

The new packager verifies the pinned Swift sources/header and each input's
detached content signature against a caller-supplied certificate fingerprint.
It checks thin Mach-O architecture, platform, deployment minimum and the same
547 C API exports. Test-only ADBC bridges are rejected. It creates universal
Mac and simulator frameworks plus the device framework, signs the frameworks
and XCFramework, and checks the resulting XCFramework platform/architecture
index. Inputs are reauthenticated after packaging. The output is a relocatable
Swift package using the pinned upstream Darwin client sources.

Pass each input as `--slice NAME=SIGNED_ROOT`, using these names:
`macos-arm64`, `macos-x86_64`, `ios-arm64`, `ios-simulator-arm64` and
`ios-simulator-x86_64`. Each root must authenticate `lib/libduckdb.dylib`.
Other required arguments are `--source`, `--out` and
`--certificate-sha256`; `--check-only` verifies inputs without packaging.
Output directories must be new. Use external SSD output and temporary paths.

On 2026-10-30 all five existing signed slices passed preflight with macOS
minimum 11.0 and iOS minimum 15.0. Evidence is under
`.context/artifacts/platform-matrix-20261029/work/apple/swift-five-slice-preflight-1/`.
These slices come from previously recorded compilation snapshots; this is
candidate input evidence, not proof that every final release slice shares the
final source state.

Full packaging completes on2026-10-30 after the canonical Intel corpus pair and
independent metadata controls. Output is `work/apple/swift-five-slice-package-1/`
within the same external artifact task. The successful driver terminal and
`summary.json` record51 package payload files. All three frameworks and the
XCFramework verify their native signatures, the resulting index contains exactly
the five planned architectures/platforms, and original inputs reauthenticate.
This is packaging evidence, not packaged-consumer runtime acceptance.

Packaging does not establish Swift consumer builds, simulator/device execution,
storage/WAL compatibility, Apple provisioning, notarization or distribution
approval. Those remain separate final gates. The original external-device-set
creation failure is preserved; the authorized dedicated internal device set now
runs the arm64 consumer and typed persistence checks described below. The
installed simulator runtime executes arm64 only.

The full-suite `build.py` harness now accepts an explicit macOS `--triple`, checks
that it matches the selected native library, and verifies the linked artifacts'
architecture. `test.py` carries that target into the test invocation and requires
a matching storage probe. Fresh pinned C++ and Rust Intel test packages are
prepared under `work/apple/swift-intel-{cpp,rust}-package-1/`.

The original suite attempt requested macOS11, which the installed Swift SDK
rejects; its supported deployment range starts at12. The next build targets12
without changing the engine slices' minimum11. Both full suites then build/sign,
but SwiftPM's arm64-only testing helper cannot load the Intel bundles. Those
failed attempts are retained under `swift-intel-full-{1,2}/`.

The harness now launches the unchanged complete bundle through Apple's universal
XCTest runner under Rosetta when targeting Intel from an arm64 host. Both engines
pass132 upstream cases plus the separate identity case, with exact133-case IDs,
selected library identity and before/after native manifests verified. Evidence:
`work/apple/swift-intel-full-3/verification.json`; the signed binaries remain in
`swift-intel-full-2/`. The explicit compiler target is macOS12, while execution is
on the current host; older operating-system runtime support is not inferred.
This proves the dylib-backed Swift suite, not the packaged XCFramework consumer.

## Packaged Mac consumers and typed persistence

The historical five-slice SDK has a detached content signature covering all 51 package
payload files plus its receipts, public certificate and recorded symlink layout.
This authenticates Swift sources and package metadata as well as the signed native
frameworks. `package_identity.py` verifies the complete payload set and symlinks.

Fresh independent SwiftPM clients build from relocated SDK copies for arm64 and
x86_64, with explicit macOS 12 compiler targets. Both run successfully, then their
signed executables/frameworks are copied away from SwiftPM into separately signed
standalone distributions and run again. Actual loaded-image identity, prepared
statements, Appender, rollback, decimal/BLOB/timestamp/list/struct/Unicode values,
checkpoint/reopen and detached-result ownership checks pass. Original and copied
payload hashes remain unchanged. The harness now asks SwiftPM for its binary
directory instead of assuming the older build layout.

Independent C++ Swift controls use the same pinned client and consumer assertions.
Each Mac architecture passes four checkpoint/nonempty-WAL journeys, both writer
directions: the harness records writer identity before intentional WAL termination,
keeps before-replay snapshots, verifies receiving-engine mutation/checkpoint, then
reads back through the original engine. All eight journeys pass. C++ WAL snapshots
are 558 bytes and Rust snapshots are 481 bytes on each architecture. Source, native
signatures, detached manifests and deployed symlink layouts verify throughout.

The arm64 C++ control uses the preserved supplied 1.5.6-source build whose literal
version is `v0.0.1`; Intel literally reports `v1.5.6`. These values remain recorded,
not rewritten. Loader IDs are normalized only in separate reference-control copies.
Arm64 runs natively; Intel runs under Rosetta. Neither proves older OS versions,
iOS execution, final common-source artifacts or performance acceptance.

Evidence under the same external artifact task:

- `swift-macos-sdk-consumers-1.json`: both relocated and standalone Mac consumers.
- `work/apple/swift-package-{arm64,intel}-{consumer,standalone}-1/`: signed payloads,
  source/identity records and actual build/run results.
- `work/apple/swift-cpp-package-controls-1/`: separate pinned C++ Swift controls.
- `macos-swift-sdk-journeys-verification-1.json`: eight complete typed journeys.
- `work/apple/swift-sdk-{arm64,intel}-journeys-1/`: unchanged deployments, original
  process terminals and before-replay database/WAL snapshots.

At this historical Mac checkpoint, iOS consumer runtime was unverified. The later
arm64 simulator receipts below provide that additional evidence. Full iOS suites
and physical-device verification remain open. The package's immutable build
receipt still says runtime is unverified by packaging; separate runtime receipts
establish additional evidence without rewriting that receipt.

## iOS package consumer compilation

All three planned iOS consumers compile and link from relocated copies of the
complete signed SDK: arm64 device, arm64 simulator and x86_64 simulator. The same
`Consumer.swift` source and typed assertions are retained. Builds use the installed
iPhoneOS/iPhoneSimulator 27.1 SDK paths, explicit iOS 15 target triples and one
compiler job. Each executable's Mach-O architecture/platform/minimum is checked;
the product framework bytes exactly match the intended signed XCFramework slot.
SDK, copied inputs, native signatures and detached consumer manifests verify.

`consumer.py` now accepts `--apple-sdk` and `--build-only` for explicit iOS
targets. iOS builds require both flags, preventing accidental execution as a Mac
binary. Build-only receipts explicitly set `runtime_verified=false` and leave
`run_exit_code` unset. A fresh Mac consumer build/run after this harness extension
also passes, preserving the desktop path.

Evidence:

- `ios-swift-sdk-consumers-verification-1.json`: all three platform/framework audits.
- `ios-swift-sdk-consumers-build-1.json`: exact commands and successful terminals.
- `work/apple/swift-ios-{simulator-arm64,simulator-intel,device-arm64}-consumer-1/`:
  copied sources, signed consumers, native metadata and raw build logs.
- `swift-package-arm64-consumer-2-terminal.json`: successful Mac regression check.

These are compile/sign gates only. No iOS execution, physical device provisioning
or Apple distribution approval is inferred. A read-only refresh of the dedicated
SSD simulator set still reports no devices (`ios-simulator-device-inventory-2.json`).
The earlier SSD device-creation failure is preserved. The subsequent owner
authorization and actual internal-device-set runtime checks are recorded below.

## Authorized internal simulator runtime

The owner explicitly permits internal storage for iOS simulator work and other
work blocked on the external drive. The dedicated device set
`/Users/amirbaron/Library/Developer/CoreSimulator/DuckDBConductor-50198a764d3b1fdc`
creates and boots an iPhone 17 Pro on iOS 26.5. Other device sets and privacy
settings are unchanged. Runtime payloads/fixtures use a separate owned internal
probe directory; logs, receipts and database/WAL snapshots remain on the SSD.

The actual arm64 simulator consumer passes prepared statements, Appender,
rollback, checkpoint/reopen, result ownership, Decimal, BLOB, timestamp,
nullable-list and Unicode-struct checks. Its sole loaded engine is the expected
signed Rust framework, with unchanged source/runtime payload hashes.

Four cross-engine typed journeys also pass between the real iOS simulator Rust
consumer and the signed supplied-1.5.6-source C++ Mac arm64 Swift control:
checkpoint and nonempty committed WAL in both writer directions, receiving-engine
mutation/checkpoint and original-engine readback. C++ WAL snapshots are 558 bytes;
Rust snapshots are 481 bytes. The harness validates the owned native iOS writer's
process path before SIGKILL, preserving committed WAL rather than triggering a
normal close. Before-replay snapshots and original process results are retained.

Evidence under `.context/artifacts/platform-matrix-20261029/`:

- `ios-internal-simulator-preflight-1/`: owner storage authorization, exact owned
  device set/UDID and successful create/boot records.
- `ios-internal-consumer-run-1/verification.json`: actual simulator typed checks.
- `ios-cpp-typed-journeys-1/verification.json`: four cross-engine journeys and
  untouched signed inputs; subdirectories retain database/WAL snapshots.
- `work/apple/swift-arm64-only-package-1/`: signed three-slice SDK produced without
  Intel inputs. Historical five-slice artifacts are preserved separately.

This closes the initial arm64 simulator runtime/typed persistence checkpoint,
not full simulator SQL/C API/ADBC/Swift-suite verification or physical-device
provisioning. Physical iOS-device execution and final common-source release and
performance gates remain open.

## Full simulator Swift suite inputs

`prepare-ios-suite.py` now prepares all nine pinned upstream Swift test files
against a separately copied, fully authenticated Apple SDK. The test bodies and
Foundation fixture helpers remain byte-identical to the upstream pin. The
additional identity case recognizes both `libduckdb.dylib` and the packaged
`Cduckdb` framework, and still requires exactly the expected native engine image.

The actual prepared package passes SwiftPM's manifest parser, and Xcode lists
the `DuckDBReleaseTests-Package` scheme. Original and copied SDKs authenticate
all 55 signed payloads before and after preparation. A separate signed input
manifest authenticates 88 payloads, including the copied SDK, complete test
sources, package manifest and preparation receipt. Python AST, source whitespace
and Swift identity-test syntax parsing also pass.

Evidence under `.context/artifacts/platform-matrix-20261029/`:

- `work/apple/ios-full-swift-suite-inputs-1/`: authenticated SDK/test inputs.
- `ios-full-swift-suite-inputs-verification-1.json`: signed input verification.
- `ios-full-swift-suite-manifest-1/`: successful SwiftPM manifest parsing.
- `ios-full-swift-suite-xcode-list-1/`: actual Xcode scheme inventory.
- `ios-full-swift-suite-identity-parse-1/`: syntax-parser result.

These are input and metadata checks. No engine/test compilation or test
execution occurred, and the simulator remained shut down. The complete
132 upstream cases plus one identity case still require simulator build,
native signing, execution and exact case-coverage verification. This SDK also
retains its historical source identities; final common-source artifacts and
physical-device verification remain separate requirements.

## Refreshed arm64 SDK and complete simulator suite

The source-43 refresh rebuilds all three required arm64 slices from the verified
C API validity/streaming repairs: macOS, iOS simulator and iOS device. Each build
and its full 2,306-file source guard passes. The slices retain the expected Mach-O
platform/minimum and identical 547 C API exports, and their native/content
signatures verify. The new three-slice Swift SDK also authenticates all 55 payloads.
Historical Intel slices and older signed SDKs remain unchanged.

All nine upstream Swift test files are copied byte-identically into a fresh suite
against this SDK. Xcode completes `build-for-testing`. The initial post-build
validator skips the framework because it recognizes only thin Mach-O files;
the framework is actually a valid single-arm64 universal container. That original
attempt is preserved. A fresh Products copy validates the universal slice,
checks platform/architecture and the exact SDK framework bytes, then signs the
test bundle and both framework copies. Its 23-payload content signature verifies.

The first simulator invocation rejects a split `--arch arm64` option before
starting any case. That zero-case failure remains preserved. A fresh deployment
uses the documented `--arch=arm64` form on the owned internal device set and
passes all 132 upstream cases plus the separate identity case: 133 unique cases
started and passed, no failures. The sole loaded engine is the expected embedded
source-43 Rust framework. Source/deployment payload hashes and native/content
signatures verify before and after. The dedicated iOS 26.5 arm64 simulator then
shuts down cleanly; other device sets are unchanged.

Evidence under `.context/artifacts/platform-matrix-20261029/`:

- `apple-arm64-null-stream-build-43/` and `apple-arm64-null-stream-freeze-43.json`.
- `work/apple/swift-arm64-null-stream-package-43/`: refreshed authenticated SDK.
- `work/apple/ios-full-swift-suite-inputs-43/`: 88 authenticated input payloads.
- `ios-full-swift-suite-build-43/`: successful build and original validator failure.
- `ios-full-swift-suite-freeze-43.json` and `work/apple/ios-full-swift-suite-signed-43/`.
- `ios-full-swift-suite-runtime-43/`: preserved zero-case argument failure.
- `ios-full-swift-suite-runtime-44/verification.json`: actual complete 133-case pass.

This establishes the complete selected Swift suite on the refreshed simulator
candidate. Physical iOS-device execution/provisioning, wider simulator core/C API/
ADBC checks, remaining clients/platforms and final common-source SQL/storage/WAL/
coupled performance acceptance remain separate open requirements.
