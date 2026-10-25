# Go client parity

Pin duckdb-go v2.10506.0 (commit addeaeb4ad0a9040c4f8dd70757e17e3852d5881),
the matching DuckDB1.5.6 client. Source archive SHA256:
dffa8d880adf55cf7c8cff5ed901e2be6dff50ff048d93532ed5695851dcea54.
The public API includes database/sql, Appender, Arrow, scalar/table UDFs, replacement
scans, logging, profiling, types, cancellation and errors. Inventory the complete pinned
source/tests before claiming coverage. Preserve its license and unchanged controls.

Use its documented duckdb_use_lib build tag to link only the explicit signed Rust C API
image; the default prebuilt static C++ engine is not an acceptable fallback. Build test
executables without running, verify native load commands, pin the selected image where
necessary, sign/strictly verify, and then run serialized tests. CPP controls use the same
client/dependency pins, explicit frozen C API image and test assertions. Keep all raw
per-test output, failures/skips, source/library/toolchain hashes and runtime image proof.

The installed /usr/local/bin/go is Go1.10.3amd64, while the selected module requires
Go1.24.0. Prepare an isolated official arm64Go toolchain on the dedicated SSD, verifying
its published archive checksum; do not alter the system Go installation. Go module/build/
temporary/telemetry caches and test fixture files must also stay on dedicated storage.
Explicit fixture-path adaptations must retain assertions and match both controls.

Authoritative contracts:
https://pkg.go.dev/github.com/duckdb/duckdb-go/v2@v2.10506.0
https://github.com/duckdb/duckdb-go/tree/addeaeb4ad0a9040c4f8dd70757e17e3852d5881
https://go.dev/dl/?mode=json

A successful build is preparation only. Completion also requires public API/tests,
installation/loading, ownership/concurrency/cancellation, types/UDF/Arrow, cross-engine
file/WAL journeys and documented performance/platform acceptance. Broader clients and
the whole-port speed–memory/storage gates remain open.
