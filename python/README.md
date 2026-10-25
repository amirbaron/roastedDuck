# DuckDB Python bindings

Native PyO3 bindings to this repository's Rust engine, targeting the pinned DuckDB
Python 1.5.6 contract in upstream.json. This package is under active implementation;
docs/python-api-inventory.json records the native API baseline. It is not complete
Python parity yet. The pinned fast suite has executed in full; three held Decimal
assertions, complete signature/lifetime audits, skipped dependency/version modules,
configured extension/HTTPFS, platform and broader packaging gates remain. All seven
non-fast files have run with default fixtures, and frozen native stubs now ship.
See docs/python-bindings.md for the current evidence and exact scope.
All 385 runtime entries in the pinned inventory are available. Type constructors,
tokenizer/descriptors and real CPU PyTorch/TensorFlow outputs have focused C++ comparisons;
this does not complete the dependency/platform and broader client gates.
The wheel also ships the pinned ADBC facade and native Rust initializer; all 23
upstream Python ADBC tests pass on both engines, with one-image and sequential
file/transaction/reopen journeys. Full fast reruns now have 3203 passing Rust records,
the same three held Decimal assertions and 35 skip/xfail records, without collection
errors. A separate PyArrow 21 matrix additionally enables 250 passing run-end cases;
its full fast run has 3453 passes, the same three held assertions and 36 skip/xfail
records. The original PyArrow 19 environments remain intact. Complete signature/lifetime
and final release acceptance remain open.

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

Arrow input, registration and frame scans use the native Rust Arrow scanner with an
owned external dependency copied into query trees. The identity registry is weak and
does not keep abandoned Python producers alive. Schema/protocol/single-use behavior,
live buffers, relation/view ownership and frame options have exact C++ comparison
evidence. Native Pandas/NumPy input and the complete Arrow surface remain unfinished.

Arrow readers/capsules own the native connection and result, survive closure of the
Python connection wrapper, and assemble requested batches lazily. Metadata survives
transfer; another query on the same connection invalidates the old active stream.
Exact C++ comparisons include release, cancellation/recovery and a background-thread
read. Full concurrency/protocol matrices and release performance acceptance remain open.

Pandas/NumPy inputs now scan retained arrays directly with masks, categories, timestamp
units and sampled object inference. Numeric chunks copy into native vectors without a
full Python row snapshot or an Arrow conversion; Arrow-backed Pandas dtypes follow the
upstream Arrow path. `from_df`/`query_df` and frame replacement share this implementation.
Full nested/error/API parity remains unfinished, including two retained C++ decimal
value/type discrepancies. Individual scan speed/RSS measurements are recorded in the
Pandas checkpoint; development results do not establish release performance acceptance.

Native relation joins support the six frozen Python join types and cross products;
table creation/insertion retain the original native query tree. Relation-variable scans
use subqueries with owned dependencies and connection checks, rather than exporting
through Arrow on the same connection. The complete 101-test DB-API cohort now passes;
the larger expression/relation and other Python/platform gates remain incomplete.

Python expressions now compose native ASTs for columns/constants/functions, arithmetic,
boolean/membership operations, casts, cases, lambdas and sort modifiers. Relation
selection/filtering/aggregation/sorting/joins consume those trees directly. All formerly
blocked relation/join tests collect; full UDF, remaining relation, error/overload and
platform/package parity are still required.
