# roastedDuck public preview

Documentation: https://amirbaron.github.io/roastedDuck/
Repository: https://github.com/amirbaron/roastedDuck
License: MIT, with retained third-party notices.

Native SDK: run `./bin/roastedDuck -c 'SELECT 42;'` after extraction.
`bin/roastedDuck` is an alias for the unchanged candidate `bin/rustdb` engine.
The library remains `libduckdb` for C API compatibility. Optional `.a` libraries
are distributed separately. The pinned Go sources are under `go/duckdb-go`;
use `duckdb_use_lib,duckdb_arrow` and link to this SDK's shared library.

Swift: add the extracted `DuckDBRust` directory as a local package dependency.
Keep its native artifacts and notices with the package. Physical iOS execution
and provisioning remain unverified; simulator and device builds are distinct.

Python: install the matching `roastedduck` wheel in a fresh virtual environment.
The import remains `duckdb`; do not co-install the official `duckdb` package.

The original source2551 native engine bytes are preserved. Packaging changes
and wheel metadata have separate provenance and installation checks. No new
full qualification run is implied. See `provenance.json` and `SHA256SUMS` in the
GitHub release for artifact identities. The preview uses historical development
signatures and does not claim Apple Developer ID notarization.

Known limitations: https://amirbaron.github.io/roastedDuck/compatibility/
Performance and raw rows: https://amirbaron.github.io/roastedDuck/benchmarks/

roastedDuck is independent of and not endorsed by the DuckDB Foundation.
DuckDB is a trademark of the DuckDB Foundation.
