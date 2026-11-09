import DuckDB
import Foundation
import Darwin
import MachO

enum ProbeFailure: Error { case failed(String) }
func require(_ condition: Bool, _ message: String) throws {
    if !condition { throw ProbeFailure.failed(message) }
}
let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "duckdb_library_version")!
var info = Dl_info()
try require(dladdr(symbol, &info) != 0, "native image lookup")
let image = URL(fileURLWithPath: String(cString: info.dli_fname)).resolvingSymlinksInPath().path
let images = (0..<_dyld_image_count()).compactMap { index -> String? in
    guard let pointer = _dyld_get_image_name(index) else { return nil }
    let url = URL(fileURLWithPath: String(cString: pointer)).resolvingSymlinksInPath()
    return ["Cduckdb", "libduckdb.dylib"].contains(url.lastPathComponent) ? url.path : nil
}
try require(images == [image], "exactly one selected engine image")
let version = unsafeBitCast(symbol, to: (@convention(c) () -> UnsafePointer<CChar>).self)
let identity: [String: Any] = ["function_image": image, "engine_images": images, "version": String(cString: version())]
try JSONSerialization.data(withJSONObject: identity, options: [.sortedKeys]).write(to: root.appendingPathComponent("identity.json"), options: .withoutOverwriting)
var checks: [String] = []
for cipher in ["GCM", "CTR"] {
    for format in ["v0", "v1"] {
        let path = root.appendingPathComponent("\(cipher)-\(format).duckdb").path.replacingOccurrences(of: "'", with: "''")
        let options = "ENCRYPTION_CIPHER '\(cipher)', debug_encryption_version '\(format)'"
        do {
            let database = try Database(store: .inMemory)
            let connection = try database.connect()
            try connection.execute("ATTACH '\(path)' AS sealed (ENCRYPTION_KEY 'synthetic-swift-test-key', \(options))")
            try connection.execute("CREATE TABLE sealed.samples AS SELECT 7::INTEGER i, 'שלום雪' s, 123.450::DECIMAL(18,3) d, '\\x00\\xFF'::BLOB b")
            try connection.execute("CHECKPOINT sealed")
        }
        do {
            let database = try Database(store: .inMemory)
            let connection = try database.connect()
            var rejected = false
            do {
                _ = try connection.query("ATTACH '\(path)' AS bad (ENCRYPTION_KEY 'wrong-key', \(options))")
            } catch {
                try require(String(describing: error).contains("Wrong encryption key used"), "wrong-key diagnostic: \(error)")
                rejected = true
            }
            try require(rejected, "wrong key must be rejected")
            try connection.execute("ATTACH '\(path)' AS sealed (ENCRYPTION_KEY 'synthetic-swift-test-key', \(options))")
            let result = try connection.query("SELECT i,s,d,b FROM sealed.samples")
            try require(result.rowCount == 1, "encrypted row count")
            try require(result[0].cast(to: Int32.self)[0] == 7, "encrypted integer")
            try require(result[1].cast(to: String.self)[0] == "שלום雪", "encrypted Unicode")
            try require(result[2].cast(to: Decimal.self)[0] == Decimal(string: "123.450", locale: Locale(identifier: "en_US_POSIX")), "encrypted decimal")
            try require(result[3].cast(to: Data.self)[0] == Data([0,255]), "encrypted BLOB")
        }
        checks.append("\(cipher)-\(format)-checkpoint-reopen-wrong-key")
    }
}
let report: [String: Any] = ["function_image": image, "engine_images": images, "checks": checks, "version": String(cString: version())]
let data = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
try data.write(to: root.appendingPathComponent("consumer.json"), options: .withoutOverwriting)
print(String(data: data, encoding: .utf8)!)
