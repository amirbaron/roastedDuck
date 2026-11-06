# Apple native platform checkpoint

The first iOS arm64 simulator engine builds successfully with Rust1.99.0, full
release LTO and one Cargo job. Mach-O metadata identifies arm64, iOS Simulator
(platform7), minimum iOS15.0 and SDK27.1. The installed SDK's supported deployment
range begins at15.0; older iOS versions are not verified by this build.

The frozen dylib exposes all547 public DuckDB functions and no opt-in ADBC test
bridges. Its install name is normalized from the Cargo build directory to
`@rpath/libduckdb.dylib` before signing. The dylib's native signature and the detached
content manifest both verify with the existing local identity and host-pinned
certificate. The static archive and frozen C header are included in that manifest.
This proves compilation, artifact structure and content authentication. It does
not prove simulator or physical-device execution.

The preserved initial slice is source25. A refreshed source26 simulator slice is
also built and signed, including the cache correction validated below. Its dylib
SHA256 is`f48597d6645a4021acafba438e2fec66a32288493c32b5e3954ebf328fd0d879`;
its static archive is
`ed8a00abc12fe41b6604c78c2e59cc997e476f0273c24852ab01afce51df929d`.
The arm64 iOS-device build also completes and is content-signed: its Mach-O platform
is2, minimum deployment15.0, SDK27.1 and public export count547. Its dylib SHA256 is
`9244d9eec220056c3c402310ba78b1dc1e258da4dc74b9989dee506d61481ad6`;
its static archive is
`3c7f81893e5d44691ac42136751bbd6254fe283223d68eccc548acff594447b4`.
Neither iOS slice has runtime proof or physical-device provisioning acceptance.

## Instance-cache behavior

The frozen C++ source lowercases database cache keys on Windows and all Apple
platforms, including iOS. Rust previously restricted file-path folding to macOS
and Windows; source25 corrects that platform condition to include iOS.

A further audit found that Rust returned named-memory keys before applying any
platform folding. A signed C++ macOS public C API control opens
`:memory:CaseProbe`, creates a marker and then reads42 through
`:memory:caseprobe`. Source26 applies the same platform folding to named memory.
The preserved signed Rust macOS baseline independently reproduces the mismatch:
the second query fails with “Table with name marker does not exist.” Its native
hash is01aaeb24…; both the C++ control and Rust counterexample retain their exact
native hashes and verified signatures before/after.
The signed public C API regression suite passes all three macOS arm64 cases:
exact-name sharing, platform-specific case folding and unnamed-memory isolation.
Separate C consumers compiled against the frozen public header pass both file
and named-memory case-fold controls against C++ and the corrected Rust dylib,
checking the exact loaded native image and sole engine mapping. Both images and
consumers retain their hashes and verified signatures after execution.

The complete30-file macOS C API harness also passes155 cases/797,776 assertions
against that same signed corrected Rust dylib. Each executable's loader output
identifies the selected library and each native executable signature verifies.
The harness excludes the upstream file-case test because it uses C++ filesystem
internals; the independent public C consumer covers that behavior separately.

The corrected macOS arm64 native dylib is
`29754c6a6a2b024e3fabf351576aed2cccfa7e6d5e0f9eab2a4a66c94578c7c4`.
It exposes547 public DuckDB symbols with no opt-in test bridge. This is current
macOS C API evidence; final shell, SQL, clients, persistence and performance refreshes
remain open. iOS runtime validation is still required.

## Simulator storage access

The installed iOS26.5 simulator runtime supports arm64 execution only. The SDK
also supports x86_64 simulator compilation, which must be reported separately.

Creating the dedicated simulator device set on the external SSD fails before a
device becomes available. CoreSimulatorService runs as the workspace user (uid501),
and the destination is owned by that user with mode0755. Its log nevertheless
reports `NSCocoaErrorDomain513` with underlying `NSPOSIXErrorDomain1`, “Operation
not permitted,” while copying device sample content. No simulator test has run.

An OS-access request is pending with the owner. The failed creation record and a
bounded relevant log excerpt are preserved. No internal-disk device-set fallback,
permission reset or modification of another simulator has been performed.

## Evidence

All task-generated toolchains, downloads, targets, logs and simulator data are
under the dedicated external SSD root `.context/artifacts/platform-matrix-20261029/`:

- `apple-rust-toolchain-1/`: pinned compiler and all planned Apple target stdlibs.
- `apple-sdk-preflight-1.json`, `apple-simulator-inventory-1.json`: SDK/runtime facts.
- `source-25/`, `source-26/`: platform and named-memory cache source checkpoints.
- `apple-aarch64-apple-ios-sim-build-2/`: successful actual build and Cargo JSON.
- `iossim-native-freeze-2.json`, `work/apple/iossim-arm64-native-signed-2/`: signed
  initial slice and exact identities.
- `iossim-native-freeze-3.json`, `work/apple/iossim-arm64-native-signed-3/`: refreshed
  signed source26 simulator slice.
- `ios-device-native-freeze-1.json`, `work/apple/ios-device-arm64-native-signed-1/`:
  signed source26 arm64 device slice and actual successful build terminal.
- `macos-arm64-native-freeze-1.json`, `work/apple/macos-arm64-native-signed-1/`:
  corrected signed macOS native slice.
- `work/apple/cache-probe-build-1/`: signed public C API simulator probe, not run.
- `cpp-apple-cache-control-1/verification.json`: exact signed reference behavior.
- `rust-apple-cache-baseline-1/verification.json`: preserved signed Rust counterexample.
- `work/apple/cache-sdk-run-1/verification.json`: three signed public API regressions.
- `work/apple/macos-cache-consumers-1/verification.json`: independently compiled
  C++/Rust file and named-memory controls, actual loaded images and stable signatures.
- `work/apple/capi-suite-1/verification.json`: all30 files/155 cases and native hashes.
- `iossim-device-1/`: creation failure and relevant CoreSimulator log excerpt.

The first build launcher lacked `rustc` on PATH and stopped before compilation;
the fresh second attempt completed. The first freeze attempt copied the native
payloads but stopped at a missing root license file; the successful fresh freeze
includes the pinned upstream license and relocatable install name. Those failed
attempts remain preserved and are not reported as accepted artifacts.

macOS Intel, remaining iOS device/simulator slices and runtime/Swift/persistence verification,
Linux Intel and final integrated release gates remain open. Local content signing
does not establish Apple development provisioning or physical-device execution.
