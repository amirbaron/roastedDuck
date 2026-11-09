# roastedDuck public preview

Documentation: https://amirbaron.github.io/roastedDuck/
Repository: https://github.com/amirbaron/roastedDuck
License: MIT, with retained third-party notices.

Native SDK: run `./bin/roastedDuck -c 'SELECT 42;'` after extraction.
`bin/roastedDuck` is an alias for `bin/rustdb`.
The library remains `libduckdb` for C API compatibility. SDK archives include
shared and static libraries plus the public headers. For Go, use the pinned
client source package with `duckdb_use_lib,duckdb_arrow` and link to this SDK's
shared library; see the documentation for the complete command.

Swift: add the extracted `DuckDBRust` directory as a local package dependency.
Keep its native artifacts and notices with the package. Physical iOS execution
and provisioning remain unverified; simulator and device builds are distinct.

Python: install the matching `roastedduck` wheel in a fresh virtual environment.
The import remains `duckdb`; do not co-install the official `duckdb` package.

Use the release's build provenance, qualification receipts, and `SHA256SUMS`
to identify the exact tested files. Historical benchmark and corpus results
apply to their named candidates. Development signatures do not imply Apple
Developer ID signing or notarization.

Known limitations: https://amirbaron.github.io/roastedDuck/compatibility/
Performance and raw rows: https://amirbaron.github.io/roastedDuck/benchmarks/

roastedDuck is independent of and not endorsed by the DuckDB Foundation.
DuckDB is a trademark of the DuckDB Foundation.
