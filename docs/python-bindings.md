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
Results consume native chunks lazily. Database instance reuse holds weak references;
it does not retain table blocks. Closing a parent explicitly closes its cursors;
closing the default connection permits a fresh default while the old object stays closed.

Conversions currently cover scalar/nested values, 128-bit integers, decimals, strings,
blobs, dates/times/timestamps, intervals, UUIDs, lists, structs and ordinary hashable-key
maps. Typed Value wrappers bind through their actual SQL type. This is partial coverage:
VARIANT/geometry and additional map/type/host conversion semantics remain required.
Python Unicode is decoded only at the real Python string/metadata/error boundary;
engine names and queries keep their native representation.

## Verified checkpoint, 2026-10-24

- A signed arm64 CPython 3.13 **release wheel** installs and passes the first seven
  upstream lifecycle/transaction/parameter/multi-statement tests. This was the initial
  source snapshot, before the later corrections below.
- The **current signed development wheel** matches **42/42 actual C++ Python outcomes**
  in isolated environments. These cover native scalar/nested and temporal conversions,
  integer boundaries, typed parameters, metadata/errors, incremental fetching,
  parent/cursor closure, default recreation and timezone representation. No comparison
  output is normalized. Initial 37/38 and later reports are retained.
- The broader unmodified 101-test DB-API/cursor/type cohort passes **51, fails 50, skips
  none** on Rust. The matching C++ 1.5.6 environment passes **101/101**. Every Rust
  failure is retained in `remaining-dbapi-gaps-3.json`; none was removed or marked skip.
- Installed-extension hashes, wheel RECORD regeneration, signature steps and loaded
  image inspection prove the Rust environment loads the Rust extension as its only
  DuckDB engine. The C++ oracle is in a separate environment. Exact dependency freezes
  are preserved; source, wheels, environments, caches and logs all live on the SSD.
- Native Python all-target Clippy with warnings denied and workspace/all-target
  compilation pass. The final fetch-after-close source correction is also checked in
  the subsequent workspace check recorded in the checkpoint.

The oracle caught negative TIMESTAMP_NS conversion: C++ truncates to microseconds
toward zero, not floor. Integers beyond signed/unsigned 128-bit range raise the frozen
InvalidInputException instead of silently changing type. The wider suite exposed
parent/cursor closure, default recreation, session-timezone/pytz representation and
fetch-after-close error selection; these are corrected and have real outcome evidence.

## Remaining gates

Relations/query composition (`sql`, `query`, `table`, transformations and independent
result lifetimes), expressions, UDFs, replacement scans, registration/Arrow import,
Arrow/NumPy/Pandas/Polars/Torch output, filesystems and remaining type/metadata/exception
semantics are still required. Many dictionary-result tests currently stop at missing
`.sql`, rather than a separate fetch-dictionary method. The API inventory marks only
partial implementation/evidence; no surface is declared fully complete yet.

All 275 upstream files and their parametrized cases still need full collection/running
with applicable dependencies; packaging/platform/version/lifetime and cross-engine
storage/WAL journeys remain open. Development-wheel correctness does not establish
release performance. Preserve the accepted uncached speed-memory baseline and report
individual query outliers in final acceptance. No table-cache retention was introduced.
The other selected language clients remain part of the goal.

Evidence: `.context/artifacts/python-bindings-20261024/python-foundation-checkpoint.json`,
`upstream-pin.json`, signed release/development wheels, `identity-1.json`,
`foundation-comparison-4.json`, `foundation-{cpp,rust}-4.json`,
`dbapi-cpp-1/`, `dbapi-rust-3/`, `remaining-dbapi-gaps-3.json`,
`native-clippy-3.jsonl`, and workspace check logs. Tooling lives in
`scripts/python-tests/`; build instructions are in `python/README.md`.

Contract sources: [pinned upstream release](https://github.com/duckdb/duckdb-python/releases/tag/v1.5.6)
and the [pinned PyO3 API](https://docs.rs/pyo3/0.27.2/pyo3/).
