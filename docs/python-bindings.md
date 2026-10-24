# Python binding implementation

Target: DuckDB Python **1.5.6**, upstream commit
`802a345bb627435d2a05d63cb74454fa98b60b5e`. The archive is pinned and hashed on the
external SSD. `python/upstream.json` and `docs/python-api-inventory.json` record the
contract: 414 deduplicated native module/class/member surfaces across all stubs, plus
the pure-Python frontend/value and packaging requirements. The upstream tree contains
275 test files. Completion requires the full documented API and applicable upstream
suite, not the focused subset below.

## Current implementation

`crates/python` implements a native PyO3 extension over this repository's Rust runtime.
`python/` builds an installable `duckdb` wheel with maturin 1.9.6 and PyO3 0.27.2.
No C++ DuckDB package or generic ADBC client is used to execute queries. The upstream
MIT pure-Python SQL type/value/DB-API helpers are reused with their license retained.

Implemented areas include connection/cursor ownership, execute/executemany, named and
positional parameters, incremental fetching, metadata, transactions/context managers,
interrupt/progress entry points, basic type constructors and the Python error hierarchy.
Native relation trees now support `sql`/`query`/`from_query`, qualified table names,
literal view names, filtering, projection, ordering, limits, distinct, aggregation,
aliases and duplicate-preserving union/except/intersect. Relations own their Python
connection and result cursor. Metadata binding preserves an already open stream and
its transaction; it does not execute the relation or retain table blocks.

Results consume native chunks lazily. Database instance reuse holds weak references;
it does not retain table blocks. Closing a parent explicitly closes its cursors;
closing the default connection permits a fresh default while the old object stays closed.

Conversions currently cover scalar/nested values, 128-bit integers, decimals, strings,
blobs, dates/times/timestamps, intervals, UUIDs, lists, structs and ordinary hashable-key
maps, plus maps with unhashable keys represented as key/value lists and fixed arrays
represented as tuples. Typed Value wrappers bind through their actual SQL type. This is partial coverage:
VARIANT/geometry and additional map/type/host conversion semantics remain required.
Python Unicode is decoded only at the real Python string/metadata/error boundary;
engine names and queries keep their native representation.

## Verified checkpoint, 2026-10-24

- A signed arm64 CPython 3.13 **release wheel** installs and passes the first seven
  upstream lifecycle/transaction/parameter/multi-statement tests. This was the initial
  source snapshot, before the later corrections below.
- The **current signed relation development wheel** matches **42/42 actual C++ Python outcomes**
  in isolated environments. These cover native scalar/nested and temporal conversions,
  integer boundaries, typed parameters, metadata/errors, incremental fetching,
  parent/cursor closure, default recreation and timezone representation. No comparison
  output is normalized. Initial 37/38 and later reports are retained.
- It also matches **30/30 exact C++ relation outcomes**: query composition, set-operation
  duplicates, qualified-name parsing/errors, maps/arrays, retained/closed owners,
  repeat/closed fetching and interleaved results. Both successful and failed metadata
  binding preserve a 5,000-row stream, including rows beyond the first native chunk.
- The broader unmodified 101-test DB-API/cursor/type cohort passes **88, fails 13, skips
  none** on Rust (previously 51/101). The matching C++ environment passes **101/101**.
  Remaining failures require NumPy/Pandas/Arrow output and dataframe import. They are
  retained in `relations-dbapi-rust-4/remaining-gaps.json`; no assertions were removed.
- A separate four-file relation cohort passes **65/65** on C++. Rust reports **5 passes,
  17 failures and one collection error**; `ColumnExpression` is missing, preventing the
  43-test `test_relation.py` file from collecting. All other collectable cases run;
  the error still fails the run. Query/insert/describe/replacement scans, closed-owner
  representation and default-connection error details remain visible gaps.
- Installed-extension hashes, wheel RECORD regeneration, signature steps and loaded
  image inspection prove the Rust environment loads the Rust extension as its only
  DuckDB engine. The C++ oracle is in a separate environment. Exact dependency freezes
  are preserved; source, wheels, environments, caches and logs all live on the SSD.
- Native Python all-target Clippy with warnings denied passes. Workspace/all-target
  compilation status is recorded in the relation checkpoint. Development-wheel
  correctness is not final release or performance acceptance.

The oracle caught negative TIMESTAMP_NS conversion: C++ truncates to microseconds
toward zero, not floor. Integers beyond signed/unsigned 128-bit range raise the frozen
InvalidInputException instead of silently changing type. The wider suite exposed
parent/cursor closure, default recreation, session-timezone/pytz representation and
fetch-after-close error selection; these are corrected and have real outcome evidence.

## Remaining gates

Complete relation APIs (including joins, query/view/create/insert and expression
arguments), representation, broader result lifetimes, expressions, UDFs, replacement scans, registration/Arrow import,
Arrow/NumPy/Pandas/Polars/Torch output, filesystems and remaining type/metadata/exception
semantics are still required. The API inventory marks only
partial implementation/evidence; no surface is declared fully complete yet.

All 275 upstream files and their parametrized cases still need full collection/running
with applicable dependencies; packaging/platform/version/lifetime and cross-engine
storage/WAL journeys remain open. Development-wheel correctness does not establish
release performance. Preserve the accepted uncached speed-memory baseline and report
individual query outliers in final acceptance. No table-cache retention was introduced.
The other selected language clients remain part of the goal.

Current evidence: `.context/artifacts/python-bindings-20261024/python-relation-checkpoint.json`,
`relations-comparison-4.json`, `relation-foundation-comparison-4.json`,
`relations-dbapi-rust-4/`, `relation-expanded-{cpp-3,rust-4}/`, signed
`relation-wheels-dev-4-signed/`, `relation-clippy-4.jsonl` and workspace check logs.
`relation-evidence-filename-collision.json` records accidental reuse of older foundation
run 3/4 JSON filenames. The new reports have separate names; the original comparison-4
data was restored from the unchanged foundation checkpoint. Original signed wheels,
source checkpoints and upstream logs remain; exact older outcome JSON bytes were lost.

Earlier foundation evidence: `python-foundation-checkpoint.json`,
`upstream-pin.json`, signed release/development wheels, `identity-1.json`,
restored `foundation-comparison-4.json`,
`dbapi-cpp-1/`, `dbapi-rust-3/`, `remaining-dbapi-gaps-3.json`,
`native-clippy-3.jsonl`, and workspace check logs. Tooling lives in
`scripts/python-tests/`; build instructions are in `python/README.md`.

Contract sources: [pinned upstream release](https://github.com/duckdb/duckdb-python/releases/tag/v1.5.6)
and the [pinned PyO3 API](https://docs.rs/pyo3/0.27.2/pyo3/).
