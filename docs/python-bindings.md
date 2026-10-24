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

NumPy/Pandas output (`fetchnumpy`, `fetchdf`/`fetch_df`/`df`, `fetch_df_chunk`) converts
bounded native chunks into owned, writable columns. Numeric conversion does not build
a full Python row list. Dtypes, masks, nested arrays, categorical ENUMs, timestamp units,
timezone snapshots and duplicate names follow the frozen client. Arrow table output
(`to_arrow_table`/`fetch_arrow_table`) uses the Rust Arrow C data exporter and requested
batch sizes; ownership passes to PyArrow with release guards on failures. Completed
materialized bulk results release their native rows. Neither path retains table blocks.

Arrow input (`from_arrow`, Arrow registration/unregistration and Python frame replacement
scans) now feeds the Rust Arrow scanner through the C stream protocol. Tables, batches,
readers, datasets, scanners, protocol objects and bare stream capsules have scoped
evidence. AST table references now copy an external owner without serializing it; bound
scans retain the producer while executing. The identity registry holds weak references
only. Live Arrow buffers remain live, and relation copies/views retain their inputs.
Replacement scans respect catalog precedence, enabled/external-access settings and
the frame-search option. They retain injected input calls before binding consumes their
arguments, while preserving original expressions/sampling for later rebinding.

Relation-created views retain their external input; raw SQL-created views retain C++'s
late name lookup behavior. Arrow readers/capsules are single-use where the producer is
single-use. Schema extraction prefers the schema protocol or exposed schema; protocol
objects without a schema need fresh streams as in C++.

Arrow reader/capsule output (`to_arrow_reader`, `arrow`, `fetch_record_batch`,
`__arrow_c_stream__`) exports a lazy native C stream. It owns its result cursor and a
shared native connection independently of the Python connection wrapper. Closing or
deleting that wrapper preserves transferred readers, while another query on the same
connection invalidates its previous active stream. Only a pending native chunk and the
requested Arrow batch are assembled. Materialized results use the existing Arrow
conversion path, as C++ does. Transfer preserves description metadata and distinct
closed-result errors. Reader release closes its own query; C callbacks catch native
panics rather than unwinding across the ABI.

Native Pandas/NumPy input now has its own `pandas_scan` implementation. It retains the
original object and C++-style shallow dataframe copy with deduplicated column names;
bind data captures underlying arrays, masks, categories and timestamp units. Scanning
converts only projected native chunks. Compatible numeric buffers copy directly into
native vectors with null/NaN/NaT validity handling; object types use sampled inference
and typed conversion, including nested structs/maps/lists. Native dataframe detection
does not import optional libraries just to inspect unrelated objects. Arrow-backed
Pandas dtypes use C++'s Arrow path. `from_df`, NumPy frame replacement and `query_df`
exist; relation `query` creates a temporary view and enforces its one-statement contract.
This is partial Pandas parity, with the wider object and API gaps below still open.

Relation joins now build native join trees for the six frozen Python join types,
USING strings/lists and predicate strings, plus cross products. Join-derived projections,
filters and aggregates preserve C++'s qualifier visibility; ordering creates the same
scope boundary. Alias relations use the native subquery relation semantics. `create`
and `insert_into` execute native ASTs with the original relation tree, preserving external
inputs and avoiding SQL-rendered data copies. Qualified-name parsing is shared with table
lookup. Python relation variables resolve to retained native subqueries before considering
Arrow protocol export, with C++'s rejection of another connection's relation.

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
- The **current signed host-conversion development wheel** matches **42/42 actual C++ Python outcomes**
  in isolated environments. These cover native scalar/nested and temporal conversions,
  integer boundaries, typed parameters, metadata/errors, incremental fetching,
  parent/cursor closure, default recreation and timezone representation. No comparison
  output is normalized. Initial 37/38 and later reports are retained.
- It also matches **30/30 exact C++ relation outcomes**: query composition, set-operation
  duplicates, qualified-name parsing/errors, maps/arrays, retained/closed owners,
  repeat/closed fetching and interleaved results. Both successful and failed metadata
  binding preserve a 5,000-row stream, including rows beyond the first native chunk.
- Input and ownership comparisons match **32/32 exact C++ outcomes**, including Arrow
  input types, live-buffer changes, repeated/single-use reads, producer exception class
  and stack text, views/registration, objects leaving their Python frame, and frame-search
  settings. The unmodified six-file Arrow cohort now passes **41/41** on both engines
  (previously 15, then 39 on Rust). This does not establish full Arrow API coverage.
- Reader comparisons match **30/30 exact C++ outcomes**: lazy/materialized batch sizes,
  deletion/closure of the connection, transfer, metadata and subsequent-fetch errors,
  aliases/deprecation warning, cross-connection scanning, connection reuse, deferred
  errors, interrupt/recovery, early release and a real background-thread read.
  Full concurrency, error and protocol matrices remain open.
- Four separate first-batch processes consume 1,024 rows from ranges of 1 million and
  100 million rows. Rust development-wheel whole-process peaks are 68.57/67.99 MB,
  versus C++ release-wheel 71.61/71.68 MB; incremental peaks after warmed imports are
  1.80/1.80 MB versus 1.59/1.57 MB. This is bounded first-batch scaling evidence,
  **not full-query or final release performance acceptance**. Individual timings and
  RSS values are retained in `memory-comparison.json`.
- The unmodified DB-API/cursor/type cohort now passes **101/101** on both engines,
  skipping none (Rust previously 51, then 88, then 100). The complete readonly relation
  journey uses dataframe input, union/join/project, dataframe output, query aliases,
  aggregation, table creation and insertion. No assertions were removed.
- **36/36 exact C++ join/write outcomes match**, including all six join types, USING
  lists/predicates, negative limits/offsets, qualifier visibility, schema/literal names,
  retained inputs, native relation-variable registration, cross-connection rejection,
  duplicate/missing targets and rollback after a primary-key violation.
- The five-file NumPy/Pandas cohort now passes **37/37** on both engines (previously
  26, then 33 on Rust), including all dataframe-chunk tests. Native scanning matches
  **49/51 focused C++ outcomes**; the two disagreements are the decimal cases below.
  Coverage includes numeric widths, nullable types, live/replaced buffers, stable names,
  categories, nested values, sampling fallback, timestamp units and object ownership.
- A broader five-file object/analyzer/category/stride cohort passes **75/81** on Rust,
  versus **81/81** on C++. Six failures remain: relation representation/`show`, nested
  map/type comparison behavior and the two decimal discrepancies. All failures remain
  in `pandas-object-rust-final/remaining-gaps.json`; none was skipped or normalized.
- Focused host conversion matches **75/76 exact C++ outcomes**, including native widths,
  nullable types, nested values, categorical/empty ENUMs, date/interval overflow errors,
  timestamp precision, buffer lifetime after close, requested Arrow batch sizes and
  incremental dataframe chunks. This includes 5,000-row boundaries; it is not a full
  conversion/platform acceptance matrix.
- The wider five-file relation/join cohort passes **72/72** on C++. Rust runs 22 cases:
  **16 pass, six fail, plus two collection errors**. Missing `ColumnExpression` blocks
  50 cases in `test_relation.py`/`test_joins.py`. Collectable failures require describe,
  insert-value APIs, closed-owner representation and default-connection error details.
  All errors still fail the run; no assertions or collection errors were removed.
- Installed-extension hashes, wheel RECORD regeneration, signature steps and loaded
  image inspection prove the Rust environment loads the Rust extension as its only
  DuckDB engine. The C++ oracle is in a separate environment. Exact dependency freezes
  are preserved; source, wheels, environments, caches and logs all live on the SSD.
- All-target Clippy with warnings denied passes for AST, parser, planner, Python and
  generator crates. Workspace/all-target compilation and the generator consistency
  check pass. All seven signed AST unit tests pass, including external-owner lifetime,
  omitted serialization and preservation of unbound expressions/sampling. Development-wheel
  correctness is not final release or performance acceptance.

The oracle caught negative TIMESTAMP_NS conversion: C++ truncates to microseconds
toward zero, not floor. Integers beyond signed/unsigned 128-bit range raise the frozen
InvalidInputException instead of silently changing type. The wider suite exposed
parent/cursor closure, default recreation, session-timezone/pytz representation and
fetch-after-close error selection; these are corrected and have real outcome evidence.

One focused host case remains an **unaccepted parity discrepancy**: materialize a
relation containing a nonempty string, change `arrow_large_buffer_size` before fetching,
then request an Arrow table. C++ 1.5.6 returns an empty string; Rust preserves the original
SQL value. The C++ row/dataframe control paths preserve the string. The frozen source
creates the Arrow schema before promoting the materialized result through a new
collector; a schema/array layout disagreement after the setting change is the likely
cause. Raw outcomes and source location are retained; this case has not been normalized,
removed, marked passing, or approved as a divergence. Rust snapshots query-time timezone
and Arrow properties and does not deliberately replace valid data with empty strings.

Two more **unaccepted numerical/type discrepancies** are retained in the Pandas audit.
For `Decimal('1E5')`, C++ produces DECIMAL(6,5) value 1.00000; Rust produces DECIMAL(6,0)
value 100000. For `Decimal('-Infinity')`, C++ returns positive infinity/FLOAT; Rust returns
negative infinity/DOUBLE. The frozen upstream assertions also encode these C++ results.
Rust currently preserves the represented values. These cases have not been changed into
passes or approved as divergences; raw outcomes and the failing upstream cases remain.

The numeric scan optimization was measured for speed **and** memory. For 100k/1M rows,
Rust development-wheel elapsed times fell from 7.90/85.03 ms to 1.31/6.83 ms. Whole-process
peaks stayed about 122/167 MB, dominated by the existing input/imports; query increments
were about 0.23/0.20 MB. C++ release-wheel times were 0.99/3.29 ms with similar peaks.
The 1M-row Rust case remains roughly 2.1 times slower in this individual microbenchmark;
the improvement does not certify final release acceptance. Every fresh-process result,
checksum and RSS value is retained; no average hides this outlier and no table-block
caching was added to obtain the speedup.

## Remaining gates

Complete relation APIs (including expression joins, insert/update/delete and expression
arguments), representation, broader result lifetimes, expressions, UDFs, replacement scans,
complete native Pandas/NumPy inference, nested conversion and input error semantics,
complete Arrow registration/protocol/schema/error/pushdown
coverage, complete reader/capsule/concurrency behavior, Polars/Torch output,
filesystems, helper/function-catalog compatibility and remaining type/metadata/exception
semantics are still required. The API inventory marks only
partial implementation/evidence; no surface is declared fully complete yet.

All 275 upstream files and their parametrized cases still need full collection/running
with applicable dependencies; packaging/platform/version/lifetime and cross-engine
storage/WAL journeys remain open. Development-wheel correctness does not establish
release performance. Preserve the accepted uncached speed-memory baseline and report
individual query outliers in final acceptance. No table-cache retention was introduced.
The other selected language clients remain part of the goal.

Current evidence: `.context/artifacts/python-relations-20261024/python-relations-checkpoint.json`,
`comparison-{joins-writes,pandas,reader,input,host,relations,foundation}-final-2.json`,
`{dbapi,host-expanded,arrow-upstream,pandas-object}-rust-final-2/`,
`relation-expanded-{cpp,rust}-final-2/`, signed `wheels-dev-final-2-signed/`,
`relation-clippy-7.jsonl` and workspace checks. All prior mismatches remain retained.

Earlier native input evidence: `.context/artifacts/python-pandas-20261024/python-pandas-checkpoint.json`,
`comparison-pandas-decimal-audit.json`, `comparison-{pandas,reader,input,host,relations,foundation}-final.json`,
`{dbapi,host-expanded,arrow-upstream,pandas-object}-rust-final/`, signed
`wheels-dev-final-signed/`, `pandas-clippy-10.jsonl`, workspace check and individual
`measure-*.json` records. Initial and vectorized scan wheels/results are retained.

Earlier reader evidence: `.context/artifacts/python-reader-20261024/python-reader-checkpoint.json`,
`comparison-{reader,input,host,relations,foundation}-7.json`,
`{arrow-upstream,dbapi,host-expanded}-rust-7/`, signed `wheels-dev-7-signed/`,
`reader-clippy-7.jsonl`, `reader-workspace-check-7.jsonl` and `memory-comparison.json`.
Earlier input source and all failed/intermediate runs remain on the SSD.

Earlier input evidence: `.context/artifacts/python-input-20261024/python-input-checkpoint.json`,
`comparison-{input,host,relations,foundation}-final.json`, `arrow-upstream-rust-final/`,
`dbapi-rust-final/`, `host-expanded-rust-final/`, signed final development wheel,
AST regression/unit logs, affected-crate Clippy, workspace/all-target and generator
checks. Earlier Arrow foundation source/wheel evidence and all failed runs are retained.

Earlier output evidence: `.context/artifacts/python-host-20261024/python-host-checkpoint.json`,
`comparison-{host,relations,foundation}-6.json`, `dbapi-rust-6/`,
`host-expanded-{cpp-5,rust-6}/`, signed `wheels-dev-6-signed/`,
`host-clippy-6.jsonl`, and `host-workspace-check-6.jsonl`. The original pinned source and
isolated environments remain under `python-bindings-20261024/`. Source/archive identities,
all failures and the unaccepted Arrow discrepancy are recorded in the host checkpoint.

Earlier relation evidence: `.context/artifacts/python-bindings-20261024/python-relation-checkpoint.json`,
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
