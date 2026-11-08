// swift-tools-version: 5.7
import PackageDescription

let package = Package(
    name: "RoastedDuckSmoke",
    platforms: [.macOS(.v12)],
    dependencies: [.package(path: "DuckDBRust")],
    targets: [
        .executableTarget(name: "Smoke", dependencies: [.product(name: "DuckDB", package: "DuckDBRust")])
    ]
)
