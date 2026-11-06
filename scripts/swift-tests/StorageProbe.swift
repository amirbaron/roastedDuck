// Foundation fixture storage precondition; run only after signing this executable.
import Foundation
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#endif

func canonical(_ url: URL) -> String {
    url.standardizedFileURL.resolvingSymlinksInPath().path
}

func emit(_ report: [String: Any]) {
    if let data = try? JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]),
       let text = String(data: data, encoding: .utf8) {
        print(text)
    }
}

guard CommandLine.arguments.count == 2 || CommandLine.arguments.count == 3 else {
    emit(["status": "usage: StorageProbe DEDICATED_ROOT [--dedicated-fixtures]", "data_written": false])
    exit(2)
}
let manager = FileManager.default
let root = canonical(URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true))
func contained(_ path: String) -> Bool {
    path == root || path.hasPrefix(root + "/")
}
var report: [String: Any] = ["root": root, "data_written": false]
let dedicatedFixtures = CommandLine.arguments.contains("--dedicated-fixtures")
report["dedicated_fixture_mode"] = dedicatedFixtures
do {
    guard let cache = manager.urls(for: .cachesDirectory, in: .userDomainMask).first else {
        emit(report.merging(["status": "no user caches directory"]) { _, new in new })
        exit(2)
    }
    let temporary = canonical(manager.temporaryDirectory)
    report["home"] = canonical(URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true))
    report["caches"] = canonical(cache)
    report["temporary"] = temporary
    report["environment_tmpdir"] = ProcessInfo.processInfo.environment["TMPDIR"] ?? "unset"
    if let configured = ProcessInfo.processInfo.environment["TMPDIR"] {
        report["configured_tmpdir_exists"] = manager.fileExists(atPath: configured)
        report["configured_tmpdir_writable"] = manager.isWritableFile(atPath: configured)
    }
    guard contained(canonical(cache)) else {
        emit(report.merging(["status": "base path escapes dedicated root"]) { _, new in new })
        exit(2)
    }
    try manager.createDirectory(at: cache, withIntermediateDirectories: true)
    let replacement: URL
    if dedicatedFixtures {
        guard let directory = ProcessInfo.processInfo.environment["RUSTDB_SWIFT_TEST_DIRECTORY"],
              contained(canonical(URL(fileURLWithPath: directory, isDirectory: true))) else {
            emit(report.merging(["status": "fixture environment escapes dedicated root"]) { _, new in new })
            exit(2)
        }
        replacement = URL(fileURLWithPath: directory, isDirectory: true)
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try manager.createDirectory(at: replacement, withIntermediateDirectories: true)
        report["fixture_environment"] = directory
    } else {
        replacement = try manager.url(for: .itemReplacementDirectory, in: .userDomainMask,
                                      appropriateFor: cache, create: true)
    }
    report["replacement"] = canonical(replacement)
    guard (dedicatedFixtures || contained(temporary)) && contained(canonical(replacement)) else {
        emit(report.merging(["status": "temporary or replacement path escapes dedicated root"]) { _, new in new })
        exit(2)
    }
    let marker = replacement.appendingPathComponent("rustdb-storage-probe-" + UUID().uuidString)
    let bytes = Data("duckdb-swift-storage".utf8)
    try bytes.write(to: marker, options: .withoutOverwriting)
    guard try Data(contentsOf: marker) == bytes else {
        emit(report.merging(["status": "marker readback differs"]) { _, new in new })
        exit(2)
    }
    try manager.removeItem(at: marker)
    if dedicatedFixtures {
        try manager.removeItem(at: replacement)
    }
    report["data_written"] = true
    report["marker_removed"] = true
    report["generic_temporary_redirected"] = contained(temporary)
    report["status"] = "verified selected fixture path and marker readback"
    emit(report)
} catch {
    report["status"] = "Foundation storage probe error"
    report["error"] = String(describing: error)
    emit(report)
    exit(2)
}
