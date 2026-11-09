// Additional test infrastructure; distinct from the132pinned upstream cases.
import Cduckdb
import Foundation
import XCTest
#if canImport(Darwin)
import Darwin
import MachO
#elseif os(Linux)
import TestNativeImage
#endif

final class LibraryIdentityTests: XCTestCase {
    func test_selected_native_library_image() throws {
        let environment = ProcessInfo.processInfo.environment
        let expected = try XCTUnwrap(environment["ROASTEDDUCK_SWIFT_EXPECTED_LIBRARY"])
        let output = try XCTUnwrap(environment["ROASTEDDUCK_SWIFT_IDENTITY_REPORT"])
        let function: @convention(c) () -> UnsafePointer<CChar>? = duckdb_library_version
        let pointer = unsafeBitCast(function, to: UnsafeRawPointer.self)
        #if canImport(Darwin)
        var info = Dl_info()
        XCTAssertNotEqual(dladdr(pointer, &info), 0)
        let actual = try XCTUnwrap(info.dli_fname).withMemoryRebound(to: CChar.self, capacity: 1) {
            String(cString: $0)
        }
        #elseif os(Linux)
        let actual = String(cString: try XCTUnwrap(roastedduck_test_function_image(pointer)))
        #endif
        func canonical(_ path: String) -> String {
            URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
        }
        #if canImport(Darwin)
        let images = (0..<_dyld_image_count()).compactMap { index -> String? in
            guard let name = _dyld_get_image_name(index) else { return nil }
            let path = String(cString: name)
            return ["Cduckdb", "libduckdb.dylib"].contains(URL(fileURLWithPath: path).lastPathComponent)
                ? canonical(path) : nil
        }
        #elseif os(Linux)
        let images = (0..<roastedduck_test_image_count()).compactMap { index -> String? in
            guard let name = roastedduck_test_image_name(index) else { return nil }
            let path = String(cString: name)
            return URL(fileURLWithPath: path).lastPathComponent == "libduckdb.so" ? canonical(path) : nil
        }
        #endif
        XCTAssertEqual(canonical(actual), canonical(expected))
        XCTAssertEqual(images, [canonical(expected)])
        let report: [String: Any] = ["function_image": canonical(actual), "expected_image": canonical(expected),
                                     "engine_images": images, "version": String(cString: duckdb_library_version())]
        let data = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
        try data.write(to: URL(fileURLWithPath: output), options: .withoutOverwriting)
    }
}
