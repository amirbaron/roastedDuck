# Linux arm64 Swift client checkpoint

On 2026-10-30, the pinned Swift client passes all **132 upstream XCTest cases**
against both signed Linux arm64 engines, plus one separate native-library identity
test per engine. The complete case IDs and pass verdicts match. This is a component
checkpoint, not final client or platform acceptance.

The initial build exposed a wrapper portability issue on Linux: the pinned client
uses Foundation's private Decimal storage fields and initializer. The recorded
production adaptation in `swift/patches/linux-decimal-public-api.patch` uses public
`Decimal(string:locale:)`, `significand.magnitude` and `NSDecimalString` APIs under
`#if os(Linux)`. The Darwin implementation remains unchanged. Both test preparation
and distribution-package preparation apply and record the same source adaptation;
the original upstream source pin remains immutable.

All upstream test assertions remain unchanged. Linux test infrastructure supplies
Darwin's missing `USEC_PER_SEC` constant with its exact value, a C loader-inspection
helper, and the additional identity test. The existing dedicated fixture-directory
adaptation keeps generated database files on the external SSD.

Both build parents exited zero before test payloads were frozen. Each runner and
test module is covered by a detached signed content manifest, verified with the
host-pinned certificate before and after execution. The selected engine manifests
were likewise verified before and after. The identity test resolves
`duckdb_library_version` to the exact selected engine and finds exactly one loaded
engine image. Source, engine and test-artifact hashes remained unchanged.

The native Linux environment uses the pinned official Swift image
`swift@sha256:11bb836ebbc05dbe59b28ffa6cb6ca6b452375d7a65b322c5b6c83f44d52f8c0`
(Swift 6.4, Ubuntu 22.04, arm64). Builds and tests ran sequentially with two CPU
cores, a 7 GiB memory limit and no container swap. This verifies execution in that
environment; it does not establish a broader Swift or glibc support floor.

Evidence is preserved beneath the dedicated external SSD artifact root:
`.context/artifacts/platform-matrix-20261029/`.

- `source-17/` and `source-18/`: recorded source adaptations and test infrastructure.
- `swift-{cpp,rust}-build-3/terminal.json`: actual successful build-parent exits.
- `swift-{cpp,rust}-freeze-1.json`: signed payload identities and trusted certificate pin.
- `work/linux-arm64/swift-{cpp,rust}-signed-1/`: frozen runners/modules and signed manifests.
- `work/linux-arm64/swift-{cpp,rust}-test-1/`: raw output, identity reports, case IDs,
  signatures before/after and successful process exits.
- `work/linux-arm64/swift-upstream-verification-1.json`: exact paired comparison.

The failed first and second C++ build attempts remain preserved. The first found
the private Decimal API issue; the second identified the missing Darwin constant.
Neither was reported as an engine failure or a passing runtime gate.

## Relocated package and typed persistence

The Linux arm64 component subsequently passes independent package consumers on
both engines and all four cross-engine checkpoint/nonempty-WAL journeys. The
client is built from a relocated local Swift package, then its executable and
native library are signed and copied together to a new deployment directory.
Runtime identity resolves to that deployment's `lib/libduckdb.so`, with exactly
one engine image loaded. The original package/build paths are not used to load
the engine at runtime. Signatures authenticate all40 files of each distribution,
including the package's sources/header/native image and the deployed consumer.

The two normal consumer runs cover prepared statements, Appender, a duplicate
primary-key error, rollback, checkpoint, reopen and detached-result ownership.
Every persistence journey checks exact DECIMAL, BLOB and microsecond TIMESTAMP
values, list elements/NULLs and Unicode struct fields. The WAL writers add typed
values after the initial checkpoint, proving replay rather than only checkpoint
decoding. The harness captures their loaded-image identities and nonempty WALs
before intentionally killing them. The receiver replays, mutates, checkpoints,
and the origin engine reads the amended database back. Original pre-replay DB/WAL
snapshots are retained. Both signed distributions remain unchanged after every
journey.

Reusable tooling: `scripts/swift-tests/package-linux.py` verifies an explicit signed
engine and trusted certificate pin, copies the pinned Swift wrapper/header with
the recorded Linux adaptation, and generates a manifest evaluated at its relocated
path. `consumer-linux-build.py` builds without executing the consumer and checks
the ELF basename dependency plus executable-relative library path. After host
signing, `interop-linux.py` authenticates and tests the relocated deployments.
Absolute build-time package paths may remain as fallback RPATHs; runtime identity
proves the executable-relative deployed library takes precedence in these tests.

Additional SSD evidence:

- `source-19/` and `source-20/`: package/consumer source snapshots.
- `swift-{cpp,rust}-consumer-build-2/terminal.json`: successful build parents.
- `swift-{cpp,rust}-consumer-freeze-1.json`: signed40-file identities.
- `work/linux-arm64/swift-{cpp,rust}-consumer-signed-1/`: signed distributions.
- `work/linux-arm64/swift-consumer-journeys-1/verification.json`: two normal runs,
  four typed journeys, runtime identity, original snapshots and signatures.
- `work/linux-arm64/swift-consumer-journeys-2/verification.json`: the same two normal
  runs and four typed journeys pass through the permanent `interop-linux.py` tool.
  Nonempty pre-replay WALs are558 bytes from C++ and481 bytes from Rust.

The first consumer build failed because the fixture called an internal Timestamp
initializer. The external consumer now obtains a typed timestamp through the public
query API and binds it through the public prepared-statement API. Its independent
expected microsecond value remains unchanged; this required no engine/client API
change. The failed build evidence remains preserved.

This verifies the Linux local-package and deployed-consumer paths in the pinned
Swift environment. Registry publication, package signing policy beyond the local
development identity and other Swift/glibc versions are not claimed.
macOS x86_64, iOS device/simulator and Linux
x86_64 verification, final integrated core/client gates, and coupled performance
acceptance remain open. See the [required platform matrix](release-platform-matrix.md).
