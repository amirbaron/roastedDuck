import DuckDB
import Foundation
#if canImport(Darwin)
import Darwin
import MachO
#elseif os(Linux)
import Glibc
import TestNativeImage
#endif

enum CheckFailure: Error { case mismatch(String) }
func check(_ condition: Bool, _ message: String) throws {
    if !condition { throw CheckFailure.mismatch(message) }
}
let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let file = root.appendingPathComponent("client.duckdb")

// Resolve identity before any writer can be intentionally killed by the WAL
// harness; that process must also prove which native image it loaded.
#if canImport(Darwin)
let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "duckdb_library_version")!
var info = Dl_info()
try check(dladdr(symbol, &info) != 0, "dladdr")
let imageName = String(cString: info.dli_fname)
#elseif os(Linux)
let symbol = dlsym(nil, "duckdb_library_version")!
let imageName = String(cString: roastedduck_test_function_image(symbol)!)
#endif
let image = URL(fileURLWithPath: imageName).resolvingSymlinksInPath().standardizedFileURL.path
#if canImport(Darwin)
let images = (0..<_dyld_image_count()).compactMap { index -> String? in
    guard let ptr = _dyld_get_image_name(index) else { return nil }
    let url = URL(fileURLWithPath: String(cString: ptr))
    guard ["Cduckdb", "libduckdb.dylib"].contains(url.lastPathComponent) else { return nil }
    return url.resolvingSymlinksInPath().standardizedFileURL.path
}
#elseif os(Linux)
let images = (0..<roastedduck_test_image_count()).compactMap { index -> String? in
    guard let ptr = roastedduck_test_image_name(index) else { return nil }
    let url = URL(fileURLWithPath: String(cString: ptr))
    guard url.lastPathComponent == "libduckdb.so" else { return nil }
    return url.resolvingSymlinksInPath().standardizedFileURL.path
}
#endif
try check(images == [image], "exactly one engine image")
let version = unsafeBitCast(symbol, to: (@convention(c) () -> UnsafePointer<CChar>).self)
let identity: [String: Any] = ["version": String(cString: version()), "function_image": image,
                             "engine_images": images]
try JSONSerialization.data(withJSONObject: identity, options: [.sortedKeys])
    .write(to: root.appendingPathComponent("identity.json"), options: .atomic)

func writeDatabase() throws {
    let connection = try Database(store: .file(at: file)).connect()
    try connection.execute("CREATE TABLE items(id BIGINT, label VARCHAR)")
    try connection.execute("CREATE TABLE typed(id INTEGER PRIMARY KEY, amount DECIMAL(18,4), payload BLOB, moment TIMESTAMP, xs INTEGER[], nested STRUCT(a INTEGER, b VARCHAR))")
    let typed = try PreparedStatement(connection: connection, query:
        "INSERT INTO typed VALUES (1, $1, $2, $3, [1,NULL,3], {'a':7,'b':'nested שלום'})")
    try typed.bind(Decimal(string: "123.4567", locale: Locale(identifier: "en_US_POSIX")), at: 1)
    try typed.bind(Data([0, 1, 255]), at: 2)
    let moment = try connection.query("SELECT TIMESTAMP '1970-01-01 00:00:01.000001'")[0]
        .cast(to: Timestamp.self)[0]
    try typed.bind(moment, at: 3)
    _ = try typed.execute()
    try connection.execute("INSERT INTO typed VALUES (2,NULL,NULL,NULL,NULL,NULL)")
    var duplicateRejected = false
    do { try connection.execute("INSERT INTO typed(id) VALUES (1)") }
    catch { duplicateRejected = true }
    try check(duplicateRejected, "duplicate primary key error")
    let prepared = try PreparedStatement(connection: connection, query: "INSERT INTO items VALUES ($1, $2)")
    try check(prepared.parameterCount == 2, "parameter count")
    try prepared.bind(Int64(1), at: 1)
    try prepared.bind("prépared שלום", at: 2)
    _ = try prepared.execute()
    let appender = try Appender(connection: connection, table: "items")
    try appender.append(Int64(2)); try appender.append("appended"); try appender.endRow()
    try appender.append(Int64(3)); try appender.append(nil as String?); try appender.endRow()
    try appender.flush()
    try connection.execute("BEGIN; INSERT INTO items VALUES (99, 'rollback'); ROLLBACK")
    try check(try connection.query("SELECT count(*)::BIGINT FROM items")[0].cast(to: Int64.self)[0] == 3,
              "transaction rollback")
    try connection.execute("CHECKPOINT")
}

func detachedResult() throws -> ResultSet {
    let connection = try Database(store: .file(at: file)).connect()
    return try connection.query("SELECT * FROM items ORDER BY id")
}
let mode = CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "normal"
var checks: [String] = []
if mode == "normal" {
    try writeDatabase()
    checks = ["prepared", "appender", "rollback", "checkpoint", "file-reopen", "result-ownership"]
} else if mode == "wal-writer" || mode == "checkpoint-writer" {
    try writeDatabase()
    let connection = try Database(store: .file(at: file)).connect()
    try connection.execute("SET wal_autocheckpoint='1GB'; INSERT INTO items VALUES (4, 'wal'); INSERT INTO typed VALUES (3,987.6543,'\\x0A'::BLOB,TIMESTAMP '1970-01-01 00:00:02.000003',[4,NULL,6],{'a':8,'b':'WAL 世界'})")
    if mode == "checkpoint-writer" {
        try connection.execute("CHECKPOINT")
        checks = ["checkpoint-writer"]
    } else {
    let wal = URL(fileURLWithPath: file.path + ".wal")
    let bytes = try FileManager.default.attributesOfItem(atPath: wal.path)[.size] as! NSNumber
    try check(bytes.intValue > 0, "nonempty WAL")
    try Data("ready".utf8).write(to: root.appendingPathComponent("writer-ready"), options: .withoutOverwriting)
    // The harness snapshots the committed WAL while this connection is alive,
    // then SIGKILLs this process to avoid automatic close/checkpoint.
    _ = readLine()
    withExtendedLifetime(connection) {}
    throw CheckFailure.mismatch("writer must be stopped by the harness")
    }
} else if mode == "read-write" {
    let connection = try Database(store: .file(at: file)).connect()
    try check(try connection.query("SELECT count(*)::BIGINT FROM items")[0].cast(to: Int64.self)[0] == 4,
              "cross-engine recovered row count")
    try connection.execute("INSERT INTO items VALUES (5, 'handoff'); CHECKPOINT")
    checks = ["cross-engine-read", "cross-engine-write", "checkpoint", "file-reopen"]
} else if mode == "read-final" {
    checks = ["cross-engine-readback", "file-reopen"]
} else {
    throw CheckFailure.mismatch("unknown mode")
}
let result = try detachedResult()
let typedConnection = try Database(store: .file(at: file)).connect()
let typedResult = try typedConnection.query("SELECT id,amount,payload,moment,xs[1],xs[2] IS NULL,xs[3],nested.a,nested.b FROM typed ORDER BY id")
let extended = mode != "normal"
try check(Array(typedResult[0].cast(to: Int32.self)) == (extended ? [1,2,3] : [1,2]), "typed row count/order")
let firstAmount = Decimal(string: "123.4567", locale: Locale(identifier: "en_US_POSIX"))!
let walAmount = Decimal(string: "987.6543", locale: Locale(identifier: "en_US_POSIX"))!
try check(Array(typedResult[1].cast(to: Decimal.self)) == (extended ? [firstAmount,nil,walAmount] : [firstAmount,nil]), "decimal persistence")
try check(Array(typedResult[2].cast(to: Data.self)) == (extended ? [Data([0,1,255]),nil,Data([10])] : [Data([0,1,255]),nil]), "blob persistence")
try check(Array(typedResult[3].cast(to: Timestamp.self)).map { $0?.microseconds } == (extended ? [1_000_001,nil,2_000_003] : [1_000_001,nil]), "timestamp persistence")
try check(Array(typedResult[4].cast(to: Int32.self)) == (extended ? [1,nil,4] : [1,nil]), "list first element")
try check(Array(typedResult[5].cast(to: Bool.self)) == (extended ? [true,true,true] : [true,true]), "list null element")
try check(Array(typedResult[6].cast(to: Int32.self)) == (extended ? [3,nil,6] : [3,nil]), "list final element")
try check(Array(typedResult[7].cast(to: Int32.self)) == (extended ? [7,nil,8] : [7,nil]), "struct integer field")
try check(Array(typedResult[8].cast(to: String.self)) == (extended ? ["nested שלום",nil,"WAL 世界"] : ["nested שלום",nil]), "struct unicode field")
let ids = Array(result[0].cast(to: Int64.self))
let labels = Array(result[1].cast(to: String.self))
let expectedIDs: [Int64?] = mode == "normal" ? [1, 2, 3]
    : mode == "checkpoint-writer" ? [1, 2, 3, 4] : [1, 2, 3, 4, 5]
let expectedLabels: [String?] = mode == "normal" ? ["prépared שלום", "appended", nil]
    : mode == "checkpoint-writer" ? ["prépared שלום", "appended", nil, "wal"]
    : ["prépared שלום", "appended", nil, "wal", "handoff"]
try check(ids == expectedIDs, "file reopen and result ownership")
try check(labels == expectedLabels, "nullable unicode values")
let report: [String: Any] = ["version": String(cString: version()), "function_image": image,
    "engine_images": images, "ids": ids.map { $0! },
    "labels": labels.map { $0 as Any? ?? NSNull() },
    "checks": checks + ["decimal", "blob", "timestamp", "list-null", "struct-unicode"]]
let data = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
try data.write(to: root.appendingPathComponent("consumer.json"), options: .withoutOverwriting)
print(String(data: data, encoding: .utf8)!)
