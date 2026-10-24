# DuckDB Python bindings

Native PyO3 bindings to this repository's Rust engine, targeting the pinned DuckDB
Python 1.5.6 contract in upstream.json. This package is under active implementation;
docs/python-api-inventory.json records the native API baseline. It is not complete
Python parity yet. Full relation/expression APIs, UDFs, replacement scans, host interchange
and additional conversions/packaging remain required.

Build from this directory with maturin 1.9.6 and a native Python interpreter. Keep
CARGO_HOME, TMPDIR, wheel output, environments and caches on the mounted external SSD;
the workspace target link already points there. Use CARGO_BUILD_JOBS=1 and nice -n 10.
Sign native extensions before running them; scripts/python-tests/sign-wheel.py signs
a copy and regenerates wheel RECORD. scripts/python-tests/identity.py verifies the
installed Rust extension and loaded engine images. Run unmodified pinned upstream tests
with scripts/python-tests/run-upstream.py and a fresh output directory for each run.

The upstream pure-Python SQL type/value/DB-API helper code retains its MIT license in
LICENSE-UPSTREAM. The native implementation links Rust directly and has no dependency
on the C++ duckdb package or the generic Apache ADBC Python client. Cursor fetching
consumes native chunks lazily; database instance reuse holds weak references and does
not retain table blocks. Relation composition uses native query trees, owns the Python
connection, and binds metadata without closing an existing result. Final release speed/memory acceptance remains separate from
development-wheel correctness evidence.

NumPy/Pandas output converts native chunks into owned columns without a full Python row
snapshot. Dataframe chunks consume the requested native vectors. Arrow table output
uses the native Rust C data exporter and transfers buffer ownership to PyArrow; requested
batch sizes and buffers surviving connection closure have comparison evidence. Import,
replacement scans, Arrow readers/capsules and full conversion parity remain unfinished.
See docs/python-bindings.md for the retained setting-change discrepancy against C++.
