# Swift

Download `roastedduck-1.5.6.post2-swift-apple.tar.gz` from
[Releases](https://github.com/amirbaron/roastedDuck/releases). After verification
and extraction, it contains a local Swift package named `DuckDBRust`, exporting
the familiar `DuckDB` product and module.

Add the extracted package as a local package dependency in Xcode, or declare it
in a Swift package:

```swift
.package(path: "../DuckDBRust")
```

Add `.product(name: "DuckDB", package: "DuckDBRust")` to your target's
dependencies. Then:

```swift
import DuckDB

let database = try Database(store: .inMemory)
let connection = try database.connect()
let result = try connection.query("SELECT 42 AS answer")
print(result)
```

## Platforms and verification

The Apple XCFramework contains macOS arm64, iOS arm64 device, and iOS arm64
simulator slices. Minimum deployment settings in package metadata are build
targets, not proof of execution on every OS version.

The 1.5.6.post2 package retains verified Mac and simulator 133-case receipts,
encrypted-storage checks, and Mac typed checkpoint/WAL handoffs. Its final Apple
archive is also checked after extraction and a fresh consumer build. **Execution
and provisioning on a physical iPhone or iPad remain unverified.** Treat iOS
device integration as experimental until tested on your own hardware. Intel
Mac and Intel simulator slices are outside this preview.

Linux Swift packages are distributed separately for arm64 and amd64. Use a
matching Swift toolchain and retain the package's shared library alongside your
application; consult its `Package.swift` for runtime search paths.

The upstream package emits existing Sendable/pointer warnings in its declared
language mode. Strict Swift 6 language-mode conformance is not claimed.
