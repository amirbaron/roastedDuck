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

Relation `map` now executes a native table-in/table-out function over at most 2048
input rows per call. It constructs the frozen client's NumPy-backed Pandas input,
infers output names/types with a zero-row callback or honors an explicit schema, then
checks each returned dataframe's column count, exact types/names and row bound.
The existing Pandas scanner binds and copies the returned batch into owned native
vectors; it does not execute a second SQL query or snapshot the entire relation.
Primitive inputs copy numeric buffers directly, including validity masks; other types
reuse the bounded host converter. ENUM codes, SQLNULL rejection, masked nested values
and bytearray BLOBs have dedicated reference journeys. Callback exceptions print the
traceback and set Python's last-exception attributes like the frozen client. The query
tree and bind data retain the callable/schema; the identity lookup remains weak.
Persistent callback-view and wider packaging/concurrency behavior still need review.

Type construction now accepts builtin/NumPy types, dictionaries, GenericAlias list/map
specifications and nullable/Union forms. Nested child access, ENUM/ARRAY/MAP metadata,
string collations and Python-string hashing follow the frozen client. Type names use
the native type-name parser without a synthetic SELECT. The SDK lookup preserves an
active stream and explicit transaction, and sees uncommitted catalog types. The Python
constructor still needs broader default/explicit connection and catalog-name coverage.

Python-target conversion now recurses through declared lists, maps, structs and arrays
before incompatible Python values can be coerced to a common inferred type. Typed
STRUCT shape errors and tuple positions follow the client. Python conversion selects
the first exact or convertible UNION member; the SQL cast engine retains ambiguity
errors. Native UDF integer conversion starts with BIGINT, while parameter conversion
sniffs the integer type, preserving the client's distinct UNION choices. Boolean UDF
returns enforce the declared BOOLEAN output. These remain scoped implementation gates.

The remaining frozen Python auxiliary modules are now packaged with their original
licenses: experimental Spark SQL, query-graph output, filesystem/BytesIO utilities,
Polars I/O, UDF helpers, version reporting and the typing marker. Existing Rust
frontend/type/value modules are retained. These helpers still require native method
coverage; copying them is not evidence that their complete runtime journeys pass.
With matching lightweight test dependencies, both packages collect **3212 fast-suite
cases with zero collection errors**, with identical case node IDs. Nine file-level skips remain shared dependency or
version gates (ADBC manager, PyArrow features, Torch, TensorFlow and build tooling);
they are coverage gaps, not new accepted exclusions. NumPy/Pandas/PyArrow baseline
versions were not upgraded; exact before/after dependencies are retained.

Connection/module profiling APIs now enable/disable profiling and return six frozen
formats using the native profiler/renderers. Retrieval preserves the query result and
configuration. Exhausted streams now finish their executor and flush actual operator
counters before query termination; previously streaming profiles reported zeros.
Profiler metadata uses planner cardinality estimates. Unread stream closure keeps its
cancellation path and does not finish the remaining query. Disabled/empty states,
errors, closed-owner priority and enable/disable transitions follow the frozen client.

Python `read_json` now builds native JSON scan relations for paths, path-like objects,
path lists and file-like snapshots. All 19 frozen options have ordered validation,
including explicit schemas, format/record modes, temporal formats, compression,
sampling/inference thresholds and multi-file/hive settings. Metadata binding preserves
a live stream and transaction; derived queries retain their input owners. Catalog views
preserve the frozen file-like lifetime rules. JSON handles use database-local immutable
file bytes through the existing weak object registry, with real sizes, independent
range cursors and native gzip/zstd decoding. No Python JSON parser or SQL row snapshot
is used. Uncompressed 2M-row path/memory scans cross the native parallel range boundary
with checked counts/sums at four threads.

JSON scratch reads now reserve space for actual file data and padding, then append
only bytes read from a bounded temporary window. This avoids zeroing or reallocating
large Vec tails on an EOF probe. The first attempted buffer change worsened large-file
memory; its raw runs are retained as a rejected experiment. Persistent table-block
retention is unchanged.

Relation joins now build native join trees for the six frozen Python join types,
USING strings/lists and predicate strings, plus cross products. Join-derived projections,
filters and aggregates preserve C++'s qualifier visibility; ordering creates the same
scope boundary. Alias relations use the native subquery relation semantics. `create`
and `insert_into` execute native ASTs with the original relation tree, preserving external
inputs and avoiding SQL-rendered data copies. Qualified-name parsing is shared with table
lookup. Python relation variables resolve to retained native subqueries before considering
Arrow protocol export, with C++'s rejection of another connection's relation.

`Expression` now owns a native parsed expression plus sort/null-order modifiers.
Column/constant/default/function/SQL/star/case/coalesce/lambda factories, arithmetic,
comparisons, boolean operations, membership, casts, aliases and case/order modifiers
compose native trees without rendering operands back to SQL. Relations accept these
trees in selection/projection, filters, aggregates, sorts and join predicates. Column
attribute access, type-based selection, native AST EXPLAIN and the dataframe convenience
projection/filter/order/alias/distinct functions are implemented. Full overload, parser
option, constructor/error and remaining expression and UDF overload coverage still need verification.

Python scalar UDFs now register native expression kernels with inferred/explicit types,
variadic arguments, stability/side effects, NULL handling and exception handling. Native
callbacks receive typed Python values; Arrow callbacks receive bounded ChunkedArray
batches exported/imported through the native C data interface. Registration closes an
active result but preserves explicit transactions. Function removal uses transactional
catalog removal with an explicit internal-entry permission; normal SQL drops retain
internal-entry protection. Callable owners remain live through the native kernel and
are released after removal. Duplicate names/signatures follow the frozen client.
`table_function`, connection list/map type helpers and `to_table` are implemented.
Relation `show`/`str`/`repr` share the shell's native box renderer and cache the bounded
10,000-row display independently of fetching. Display options invalidate that cache.
The string `render_mode="columns"` discrepancy below remains unaccepted.

Values now construct native expression-list relations from literal lists or variadic
expression/tuple rows, preserving inferred types, expression names and aliases.
Table `insert` keeps bind and execution errors distinct, matches the frozen execution
prefix and uses the native transaction/constraint path. `update` builds native update
statements from dictionary expressions/defaults/conditions. The frozen wrapper exposes
bind failures but discards execution results: native runtime failures still roll back
or abort the transaction, matching the C++ behavior verified by explicit journeys.
`describe` builds the same six aggregate/UNNEST rows as C++, remaining lazy over the
original relation with its retained external inputs. No Python row snapshot executes
these operations. Missing update-column hints now use the native Levenshtein candidate
helper, preserving name bytes and C++'s newline/ranking behavior.

CSV reading (`read_csv`/`from_csv_auto`) now adapts the frozen Python options into
native table functions. A metadata bind sniffs the files once, retains the resulting
schema/dialect and turns off later auto-detection, matching the native relation contract.
Strings, bytes paths, PathLike values, lists/globs and file-like objects are supported.
File-like objects are read once at their current position into immutable native bytes,
as in the frozen object-store snapshot; they do not become Python row snapshots or
internal-disk temporary files. A database-local registry holds weak file names, while
relation copies own snapshots and open buffers retain bytes independently. Catalog
views retain the file names, matching C++'s failure once an ephemeral CSV relation dies;
this differs from Arrow view ownership. Native metadata binding activates the connection's
file-opener settings and preserves its active result. External-access/disabled-filesystem
checks match, and an unregistered private scheme keeps the ordinary local-file behavior.

`to_csv`/`write_csv`, including the dataframe convenience wrapper, execute native COPY
ASTs over the original relation. Options include quoting/NULL/date/time/compression and
partition/overwrite/thread/temp-file behavior. Python argument checks follow the frozen
wrapper. Physical relation metadata also matches C++ when `filename` is requested:
fetched rows can include the extra field while the relation's metadata omits it.
`CSVLineTerminator` values and invalid/foreign enum inputs are handled explicitly.

Parquet reading (`read_parquet`/`from_parquet`) and writing (`to_parquet`/`write_parquet`)
now use native table scans and COPY. Path conversion and file-like snapshots share the
CSV adapter. Native range reads hold immutable snapshot bytes without local-disk copies;
physical file handles keep their original path/version/cache behavior. Options include
binary strings, row numbers, filename/hive/union fields, compression, nested field IDs,
row-group/file sizes, partitions, overwrite/append/thread/temp-file controls and filename
patterns. The COPY size option now uses the shared DuckDB-compatible byte parser;
`1k`, `1G`, binary units and invalid unlimited/sentinel values follow C++.

The explicit Parquet call parser preserves C++ numeric/None boolean conversion and
formats the actual positional/keyword invocation. Relation query/execute and the
frontend's default-connection type validation share the same diagnostic formatter.
Query byte strings remain native through temporary view creation and SQL parsing.
Closed aggregation rejects its closed connection before parsing expressions; many
other relation methods still need implementation, including the next `any_value` failure.

The relational aggregate/window convenience families now build native query trees for
aggregation, grouping sets/cubes/rollups, projected columns and windows. Methods include
scalar/statistical/boolean/bitwise/list/string/arg aggregates, quantiles, `apply`,
`value_counts`/`unique`, ranks, ntile, first/last/nth values and lag/lead. The expression
adapter follows frozen argument splitting, quoting fallback, separators, quantile
precision, offset/default handling and IGNORE NULLS; it does not materialize Python
rows or re-enter the SQL execution API. GROUP BY parsing copies only the group node,
so trailing ORDER BY/HAVING/LIMIT clauses have the same ignored behavior as C++.
Table-function SQL rendering now preserves C++'s omitted alias while retaining the
public function alias. Parameter iterables/mappings follow the frozen ABC checks.
Implemented relation methods reject closed owners before parsing; missing APIs remain
visible. Set operations and cross products now validate both relation owners before
reporting a connection mismatch; the broad closed-owner case passes.

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
- **64/64 exact C++ expression outcomes match**, covering native SQL text/name/repr,
  immutable composition, arithmetic/boolean evaluation, cases/lambdas, casts including
  keyword calls, errors, sorting, filtering and aggregation.
- The five-file NumPy/Pandas cohort now passes **37/37** on both engines (previously
  26, then 33 on Rust), including all dataframe-chunk tests. Native scanning matches
  **49/51 focused C++ outcomes**; the two disagreements are the decimal cases below.
  Coverage includes numeric widths, nullable types, live/replaced buffers, stable names,
  categories, nested values, sampling fallback, timestamp units and object ownership.
- A broader five-file object/analyzer/category/stride cohort passes **79/81** on Rust,
  versus **81/81** on C++. Only the two decimal discrepancies remain. Nested MAP
  promotion and typed STRUCT shape checks now pass; all remaining failures are retained
  in `python-nested-20261024/remaining-gaps.json`, without skips or normalization.
- Focused host conversion matches **75/76 exact C++ outcomes**, including native widths,
  nullable types, nested values, categorical/empty ENUMs, date/interval overflow errors,
  timestamp precision, buffer lifetime after close, requested Arrow batch sizes and
  incremental dataframe chunks. This includes 5,000-row boundaries; it is not a full
  conversion/platform acceptance matrix.
- The seven-file relation/join/expression cohort now **collects all 123 cases**: Rust
  passes **123/123**, skips none, matching C++. The previously blocked 50 cases run
  without collection errors. All 51 expression/SQL-expression cases pass; the full
  closed-owner journey now passes after implementing map and correcting set-operation
  owner validation. No assertion or collection error was removed.
- The full restored Spark compatibility directory runs **294 cases** on both engines:
  **289 pass, 4 existing skips and 1 existing xfail**. The JSON reader closes the last
  failure in this cohort; this is not complete Spark/platform acceptance.
  Unnamed STRUCTs now return positional tuples for row fetching, fixing arrays_zip;
  NumPy/Pandas retain their separate frozen mapping conversion. **18/18** typed tuple
  comparisons preserve container identity and every field. No Spark test was edited.
- JSON matches **105/109** isolated outcomes; every case and process exit remains in
  the denominator. Four oversized-integer/file-like combinations expose the reference's
  pending Python error: callback diagnostics vary, and one case aborts. Rust clears the
  pending state and returns exceptions safely. This eighth discrepancy category remains
  unaccepted; no crash, error or assertion was hidden. Binding-descriptor addresses,
  callback/internal stack frames and generated URI identifiers are the documented
  diagnostic normalization, with raw errors retained. Eleven signed native JSON tests
  pass, with three pre-existing timing probes ignored.
- The unmodified profiling helper file passes **2/2** on both engines; **37/37** focused
  profiling outcomes match after timing/resource fields are excluded from functional
  comparisons. Complete raw profiles retain every timing/resource measurement. Two
  signed streaming tests verify actual row/byte/scan counters and early cancellation;
  the two signed type-lookup tests also pass on this source. Full workspace checking
  and Python/SDK Clippy with warnings denied pass.
- Three unmodified type/conversion files pass **34/34** on both engines. **84/84**
  focused type outcomes match exactly, including constructors, errors, metadata,
  attributes, hashing, typed tuple/STRUCT parameters and duplicate-member UNION UDFs.
  Five direct SQL UNION controls also match; Python's conversion rule was applied only
  at the Python boundary. Two signed SDK tests cover live-stream lookup, failed lookup,
  uncommitted enum types and rollback, without closing an active result.
- The unmodified dataframe-map file passes **11/11** on both engines. **82/84** focused
  map outcomes match (80 exact, two with internal native stack traces excluded).
  Cases run in separate processes, with every error/process outcome retained. The two
  remaining differences are held unaccepted: the reference produces unstable incorrect
  sums after unretained numeric callback output, and crashes after a mapped view loses
  its original relation/callable owner. Rust returns the expected values and releases
  owners after dropping the view. Keeping only the latest output dataframe alive on
  both engines gives matching multi-batch results and permits checked measurements.
  Raw crash evidence and the frozen numeric scan/map source support a buffer-lifetime
  explanation; this is not a claim that all callback lifetime behavior is complete.
- The four unmodified aggregate/window/grouping/function files pass **108 cases**
  with **one existing skip** on both engines. Focused comparisons match **79/81**
  outcomes; both mismatches are the same unaccepted UTF-8 trim defect described below.
  The standalone table-function suite passes 1/1, and ten closed-state/unknown-attribute
  outcomes match C++ without stubbing missing methods or changing unknown attributes.
- The three primary Parquet files pass **57/57** on both engines. With the Arrow
  Parquet round-trip file included, both engines pass **59/60**, skipping none. The
  shared failure compares Pandas datetime64[ms] from PyArrow with datetime64[us] from
  DuckDB's TIMESTAMP reader; values match. Millisecond/nanosecond/microsecond/second
  diagnostics are retained; no assertion was modified, removed or marked xfail.
  **50 Parquet outcomes** and **19 calling-contract outcomes** match. Normalization
  is limited to independently generated object-store identifiers, object addresses,
  output work directories and language-specific internal-error stack traces, with all
  raw text retained. Internal error classes/stable diagnostic text remain compared.
- The two unmodified CSV files pass **98 cases**, with **one existing skip**, on both
  engines. **56 focused outcomes** cover path/file-like input, bytes, options/errors,
  compressed snapshots, exact output bytes, live paths versus captured data, derived/
  view/query lifetimes, metadata/SQL round trips, virtual filename metadata and access
  controls. Only generated object-store URI identifiers are normalized; raw errors are
  retained beside each normalized outcome. The native ownership/namespace test passes
  as a signed binary. No upstream assertions were weakened and no skips were added.
- **30/30 exact values/insertion outcomes**, **14/14 describe outcomes** and **23/23
  update outcomes** match C++. Coverage includes literal/expression/tuple rows, row
  shape/types/names/errors, nested values, defaults, constraints/recovery, transaction
  rollback/abort, catalog/search-path target behavior, lazy aggregates, empty/nested/
  temporal results, retained Arrow inputs, and byte-preserving candidate suggestions.
- All **seven pinned upstream UDF files** now match C++: **559 pass, one existing xfail,
  zero failures/collection errors**. This includes native/Arrow type coverage, 400 mixed
  NULL-layout cases, exceptions/side effects, removal and return-value refcount leakage.
  **26/26 focused C++ outcomes** also match: callable ownership/release, bounded Arrow
  batch sizes, annotations/varargs, errors, duplicate signatures across connections,
  explicit transaction rollback and Arrow property changes.
- Native relation rendering matches **18/19 focused C++ outcomes**. Values, Unicode,
  nested/empty results, truncation, configurable NULL text, cache/fetch independence,
  closed owners and integer/enum column mode match. One string-mode disagreement is
  retained below. No failing case was excluded from the denominator.
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

A fourth **unaccepted parity discrepancy** is recorded in relation display. C++ renders
rows when `show(render_mode="columns")` receives a string, while Rust renders columns;
integer `1` and `RenderMode.COLUMNS` agree. The frozen Python `render_mode_enum.hpp`
converter stores a pointer to a local enum in its string branch, leaving a dangling
pointer before extraction. Rust uses the parsed enum. `render-comparison-optimized.json`
retains the exact outputs; this has not been accepted as a divergence.

A fifth **unaccepted parity discrepancy** affects aggregate/window column strings.
The frozen C++ `StringUtil::RTrim` tests signed bytes with `ch > 0`, so it removes
trailing UTF-8 bytes. `sum("雪")` becomes an empty quoted identifier and raises a parser
error. With columns `prefix=11` and `prefix雪=99`, `sum("prefix雪")` returns **11** on C++
and **99** on Rust. Rust trims ASCII whitespace and preserves the requested identifier.
Explicitly quoted Unicode columns match both engines. Raw results remain in
`aggregation-comparison-current.json`; no case was normalized, excluded or accepted
as a divergence. This has not been converted into a passing comparison.

The numeric scan optimization was measured for speed **and** memory. For 100k/1M rows,
Rust development-wheel elapsed times fell from 7.90/85.03 ms to 1.31/6.83 ms. Whole-process
peaks stayed about 122/167 MB, dominated by the existing input/imports; query increments
were about 0.23/0.20 MB. C++ release-wheel times were 0.99/3.29 ms with similar peaks.
The 1M-row Rust case remains roughly 2.1 times slower in this individual microbenchmark;
the improvement does not certify final release acceptance. Every fresh-process result,
checksum and RSS value is retained; no average hides this outlier and no table-block
caching was added to obtain the speedup.

The UDF microbenchmarks also retain speed and memory together, one fresh process per
case and one engine thread, with a Rust development wheel versus C++ release. A native
primitive-return optimization removes intermediate vector casts and reuses only bounded
argument buffers. Native 100k/1M execution fell from 27.9/271.9 ms to **8.3/80.0 ms**;
C++ took **6.5/57.4 ms** in the repeat controls. Whole-process peaks stayed about
116–117 MB for both engines. Arrow 100k/1M took **5.3/50.0 ms** versus **5.1/35.9 ms**,
with about 118.5 MB Rust versus 125.5 MB C++ peak RSS. The 1M native and Arrow cases
remain about **39% longer** individually; they are not hidden in a suite average or
accepted as final release performance. Raw before/after timings, checksums and memory
are in `performance-comparison{,-optimized}.json`. The storage table-cache policy is
unchanged; no residency increase was used to close the callback speed gap.

CSV speed and memory were measured in separate fresh processes at one engine thread,
including relation construction/sniffing plus aggregation. Three runs remain visible;
these are Rust **development** versus C++ **release** measurements, not final acceptance.
For 1M rows, path reads took Rust **76.0–78.4 ms** versus C++ **57.0–59.8 ms**; file-like
reads took **75.8–78.7 ms** versus **57.3–58.2 ms**. These individual slowdowns remain open.
Path whole-process peaks were Rust **102.7–106.7 MB** versus C++ **75.0–75.5 MB**; the warmed
pre-query baselines were about **91 MB** versus **60 MB**. Path query increments varied
from **12.0–15.7 MB** versus **14.7–14.9 MB**. For file-like input, peaks were **131.4–131.7
MB** versus **118.0–121.6 MB**, with increments **26.4 MB** versus **43.5–47.1 MB** after
preparing the caller's BytesIO. Whole-process memory is higher despite the lower
file-like query increment; neither metric is hidden. All four size/mode cases, CPU
measurements and checksums are retained in `performance-comparison{,-final,-current}.json`.
The accepted TPC-H/ClickBench baseline and table-cache policy remain unchanged.

Parquet speed and memory were also checked in two fresh-process runs at one engine
thread (Rust development versus C++ release), including relation construction and
aggregation. For 1M rows, paths took Rust **14.1–14.4 ms** versus C++ **12.6–13.1 ms**;
file-like input took **14.7–14.9 ms** versus **13.9–14.1 ms**. Path peaks varied **133.9–134.0
MB** versus **131.7–137.0 MB**; file-like peaks were **157.8–157.9 MB** versus **170.2–170.7
MB**. File-like query increments were **18.1–18.2 MB** versus **31.5–31.6 MB**. The 100k-row
path case varied from Rust 27% shorter to 21% longer elapsed time across these small measurements;
raw individual cases/CPU/checksums/peaks remain in `performance-comparison{,-verified}.json`.
This is not final release acceptance and does not change the accepted engine baseline.

The grouping/window wrapper measurements also report speed and memory together,
one fresh process per query at one engine thread (Rust development, C++ release).
For 1M generated rows and 100 groups, grouping took Rust **11.4–11.6 ms** versus C++
**5.4–5.5 ms**, at **44.4–44.5 MB** versus **48.6–48.9 MB** peak RSS. The window query took
**121.9–123.6 ms** versus **90.3–91.1 ms**, at **75.2–75.9 MB** versus **80.7–82.1 MB**.
Direct SQL reproduces the same individual slowdowns (11.4 vs 5.4 ms and 121.6 vs 89.7 ms),
so the wrapper is not responsible. The 100k-row window case remains about 1.7–1.8 times
slower; all four size/mode cases and checksums are retained in
`performance-comparison{,-current,-sql}.json`. No cache retention or memory increase
was used to close these gaps; they remain open for final performance acceptance.

Dataframe-map speed and memory were checked in two fresh processes per size/mode,
one engine thread (Rust development, C++ release). The first 1M-row identity path took
Rust about **60 ms**; bounded direct numeric conversion reduced it to about **42.5 ms**,
with C++ about **38–39 ms**. Adding one column took Rust about **68 ms** versus C++
**62–63 ms** after the change. Whole-process peaks remained about **117 MB** Rust versus
**126–128 MB** C++, with only a bounded output frame retained on both engines to avoid
the reference's observed numeric-output lifetime defect. The 100k-row timings and all
repetitions remain in `performance-comparison{,-optimized,-current}.json`. These scoped
measurements do not certify final release performance; no table-cache policy changed.
A three-row prefix consumes one 2048-row input batch for both 1M and 100M declared
rows, at about 117 MB peak RSS on both engines. Python all-target Clippy with warnings
denied and the complete workspace/all-target check pass on the signed-wheel source.

Fresh UDF controls after the type/conversion work retain individual speed-memory
outliers: native 1M-row Rust took **83.2–86.0 ms** versus C++ **57.3–57.7 ms** in the
repeat controls, at about **116–117 MB** for both. One noisy C++ run took **102.4 ms**;
that raw run remains visible and is not used to imply a speed advantage. Arrow took
Rust **54.6–58.8 ms** versus **42.2–46.4 ms**, at about **119 MB** versus **125–126 MB**.
All checksums passed. These are development/release microbenchmarks, not final release
acceptance or proof of a new regression against the earlier development wheel. No
memory retention was introduced to close these individual speed gaps.

JSON measurements keep speed and memory paired, one fresh process per case (Rust
development versus C++ release). Before the scratch fix, the 1M-row local scan took
about **45 ms / 104 MB** peak RSS. The corrected runs took roughly **39–43 ms / 59–60
MB**; C++ varied about **33–40 ms / 62–63 MB**. The 2M-row four-thread path and memory
scans and prefix cases retain every result checksum, elapsed/CPU time and RSS value.
The local prefix scan remains a substantial individual slowdown (roughly 16–20 ms
versus 9–11 ms); the 100k local case is also slower. File-like paths show different
speed/memory tradeoffs, so suite averaging does not stand in for acceptance. Initial,
rejected, corrected, repeat and final-wheel records are preserved under python-json.
These development measurements are not final release performance acceptance.

## Polars and replacement retention checkpoint (2026-10-25)

Native eager and lazy Polars conversion/input now use native Arrow and the unchanged
frozen `polars_io` helper. Lazy relation schema inspection does not execute data;
LazyFrame input remains deferred and repeatable. Eager inputs retain a single Arrow
snapshot. Connection lazy results preserve the reference's narrower result-only
operations and stream-closure diagnostics. No table-block caching is introduced.

The unmodified Polars cohort passes **58 cases with two existing version skips** on
both engines. The replacement-scan/dependency-leak cohort now passes **68/68** on both,
up from Rust 42/68. Binding captures injected external calls before scalar subqueries
and cloned CTE definitions consume their parse trees, and retains these calls in the
original unbound relation. Generated table references receive a stable location.
This fixes table/frame ownership, nested/recursive CTEs, CTE shadowing, scalar
subqueries and later catalog-table creation without retaining whole result data.
The five existing million-row ownership/leak cases also pass.

Sixteen scoped regression cohorts run 1681 cases: **1668 pass, 3 fail, 10 skip/xfail**,
with no collection errors. The three failures are unchanged: two held decimal
reference discrepancies and the shared Parquet timestamp-unit assertion. Spark
remains 289 pass/4 existing skips/1 existing xfail. These are component executions,
not a full Python-suite pass; do not add them to older checkpoint counts.

Focused Polars outcomes match **73/74**. The remaining zero-batch eager relation
case exceeds the 512 MiB C++ process guard; Rust returns a bounded InvalidInputException.
This is a ninth **unaccepted** discrepancy category. C++ raw exits/RSS and Rust
outcomes are retained. Composed lazy journeys match 25/26; the same-connection
self-join remains open. Across 12 repeats C++ consistently reports a closed query
stream, while Rust returns a busy-connection error 8 times and correct rows 4 times.
Do not classify this concurrency gap as an accepted skip or silently normalize it.

Coupled fresh-process measurements retain elapsed/CPU time, whole/incremental RSS,
checksums and thread settings for every case. Eager 1M-row Polars output remains an
individual outlier: Rust 4.16–4.42 ms/122.5–122.7 MB versus C++ 1.71–1.91 ms/112.3–113.2 MB.
The Rust generic materialized-result/Arrow path differs from the C++ Arrow collector;
a future optimization must preserve materialization/error ordering and compare both
speed and memory. A lazy 1024-row prefix remains bounded when source cardinality grows
from 1M to 100M (about 117 MB process peak). Noisy first C++ lazy/filter timings remain
in the raw report alongside the repeats. These development-wheel microbenchmarks do
not certify final release performance or replace the accepted uncached suite baseline.

Evidence: `.context/artifacts/python-polars-20261024/python-polars-checkpoint.json`,
`regression-matrix-retention.json`, `remaining-gaps-retention.json`, replacement
before/intermediate/after receipts, `polars-comparison-current.json`, isolated process
receipts, lazy/shared-connection repeats, `performance-comparison-retention.json`,
signed wheels and native compiler/check/test records. Older failed experiments remain
preserved. Full Python/client/platform and final core/shell/storage/WAL/release gates
remain open.

## Explicit/default connection and value checkpoint (2026-10-25)

An additional eleven-file upstream cohort now passes **126/126** on both engines,
closing nine Rust failures. Module-level `df` supports both result fetching and
DataFrame input; `aggregate` and `limit` honor explicit/default connections. Their
frozen overload diagnostics retain actual argument order and representations,
including the reference's duplicate `df` fetch signatures. Empty positional calls
with keywords now use the reference diagnostic separator.

Closed-result Arrow consumers report the reference's missing-query-result error,
while row/NumPy consumers keep the closed-result error. Explicit BIT bytes parse
text bits rather than treating ASCII bytes as binary payload; malformed bytes keep
the native error bytes through the Python Unicode boundary. Explicit float-to-decimal
conversion is rejected, typed NaN becomes NULL, and datetime precision/timezone
behavior follows the frozen conversion dispatch. Decimal and other intrinsically
typed Python objects preserve that dispatch rather than applying an extra SQL cast.
Ordinary SQL cast behavior is unchanged.

All **56 focused runtime outcomes** and **84 type outcomes** match, with only live
connection/relation addresses normalized and raw errors retained. The Polars
recheck remains 73/74 with its existing held zero-batch resource discrepancy.
Seventeen current development-wheel cohorts execute 1807 cases: **1794 pass, 3 unchanged
failures, 10 skip/xfail**, with zero collection errors. The failures remain the two
held decimal reference discrepancies and shared Parquet timestamp-unit assertion.
Current Python all-target Clippy with warnings denied and workspace/all-target
checking pass. This is broader component coverage, not full Python/runtime/platform
or final release acceptance. No table caching or performance-target change is added;
previous individual performance outliers remain open.

Evidence: `.context/artifacts/python-runtime-20261025/python-runtime-checkpoint.json`,
signed `wheels-dev-2-signed/`, `runtime-comparison-2.json`, raw probes,
`types-comparison-current.json`, `regression-matrix-current.json`,
`remaining-gaps-current.json`, isolated Polars rechecks and compiler terminal records.
The complete source archive preserves the still-uncommitted migration and bindings.

## Append, threading and Python persistence checkpoint (2026-10-25)

Native connection/module `append` now registers the dataframe as `__append_df` and
executes a native INSERT. It preserves the frozen registration after success or
failure, the live shallow dataframe snapshot, explicit/default connection ownership,
column ordering/by-name defaults, return cursor and overload diagnostics. Unicode,
quoted and dotted literal table names remain identifiers. It does not convert the
whole dataframe to Python rows or call the C++ engine.

A shared atomic closed flag makes interrupt/progress reject closed handles, including
children closed by their parent, without acquiring the connection's execution mutex.
An active query can still be interrupted from another thread. The six-file scoped
threading/lifecycle/default-append cohort passes **41/41 on both engines**, closing
the two failures in the preceding 40-case run. All **52 focused append/lifecycle
outcomes** match, with live object addresses alone normalized and raw errors retained.
Current Python all-target Clippy with warnings denied and workspace/all-target checks
pass. Eighteen current development-wheel cohorts run **1848 cases: 1835 pass,
3 unchanged failures, 10 skip/xfail**. The three failures remain the two held decimal
reference cases and shared Parquet timestamp-unit assertion.

The actual Python append persistence journey passes in both directions for checkpoint
and crash-left nonempty WAL files. It covers 15 columns: integer, Unicode/NUL string,
blob, decimal, timestamp, boolean, date, LIST, ARRAY, STRUCT, MAP, UUID, BIT, INTERVAL
and ENUM. Initial data matches the C++ control; the opposite engine replays/reads it,
inserts a fourth row, checkpoints, and the original engine reads that row back.
Both WAL controls are 999 bytes; their hashes and pre-recovery files are retained.
Do not call the files byte-identical or extrapolate this journey to every type,
corruption/recovery, encryption or version/platform combination.

The larger Polars concurrency audit corrects the earlier 12-repeat observation. In 100
runs sharing one relation, C++ returns 73 closed-stream errors, 14 closed-pending errors
and 13 incorrect results. Two relations sharing a connection return 81 closed-stream
errors, 10 closed-pending errors and 9 incorrect results. Rust returns busy errors or
correct rows in these shared-context probes. Both engines return correct results in
all 100 independent-connection and 100 sequential-concat runs. Raw incorrect-row outputs
and compact count/hash receipts are preserved. This becomes a **tenth unaccepted
reference discrepancy category**, not a reason to fabricate failures or reproduce
corruption. The shared-context concurrency acceptance question remains open; stream
locking was not changed merely to force a particular raced outcome.

Coupled result-checked append benchmarks retain time and RSS for every case. A 1M-row
positional append takes Rust 5.84–5.88 ms/145.6–145.8 MB versus C++ 3.04–3.72 ms/
143.2–143.8 MB. By-name timings and the noisy 100k case are retained separately. These
are development measurements, not final release acceptance or changes to the accepted
uncached TPC-H/ClickBench baseline. No table cache is added.

The coverage receipt identifies **1372 collected cases in 131 files** outside the
current component cohorts. Some scoped cases are explicitly run from a file outside
the normal collection pattern, so do not subtract 1848 from 3212 to infer coverage.
The nine shared dependency/version file skips and missing-environment matrix remain
additional coverage gates. Earlier focused/custom checks do not establish execution
of these unmodified upstream cases.

Evidence: `.context/artifacts/python-streams-20261025/python-streams-checkpoint.json`,
`append-comparison-current.json`, current regression/coverage receipts,
`controls-summary-verified.json`, raw concurrency receipts, `persistence-current/`,
`performance-comparison.json`, signed wheels, compiler terminal records and the complete
source archive. Previous runtime and Polars controls remain preserved.

## Parsed statements and connection metadata checkpoint (2026-10-25)

The complete upstream connection API file passes **48/48 on both engines**, closing
six Rust failures. Native `Statement` objects own immutable parsed ASTs and expose the
frozen query/type/parameter/result metadata. Execution, query and batch APIs accept
these objects without serializing/reparsing their ASTs. Shared native preprocessing
handles PRAGMA/PIVOT expansion. Table-name discovery and extension install/load APIs
use the SDK; connection construction now follows the frozen path/default-option and
boolean rules. Invalid/empty query handling and diagnostics match the scoped controls.

`executemany` prepares its final statement once, executes preceding statements once,
materializes the outer parameter iterable before execution, and retains the final
result. This matches the frozen client, including generator-failure ordering. No
cross-call prepared cache or table-block retention is introduced.

All **131 focused statement/metadata outcomes** match, with raw diagnostics preserved;
only live object addresses and internal backtrace text are normalized. Eight signed
SDK tests pass, including four new metadata tests. They cover active stream survival,
explicit transactions/rollback, owned PRAGMA preprocessing and external-access
configuration in direct loading. The native load helper snapshots current settings;
metadata preprocessing preserves an existing automatic stream transaction.

Statement/result enums use the frozen unsigned-byte domain, unknown-value spelling,
strict type equality, member dictionaries and allocation/state initialization rules.
Eight cross-engine pickle journeys pass for protocols 2–5, each carrying 36 declared or
unknown enum values. C++ protocol 0/1 probes abort with exit -6; Rust safely round-trips
those protocols. Raw process exits are retained as an **eleventh unaccepted reference
category**, not an accepted skip or a reason to reproduce an abort.

Nineteen current development-wheel cohorts run **1895 cases: 1882 pass, 3 unchanged
failures, 10 skip/xfail**, with zero collection errors. Existing failures remain two
held decimal reference discrepancies and the shared Parquet timestamp-unit assertion.
The overlapping append node was removed from the threading cohort. Python/SDK
all-target Clippy with warnings denied and workspace/all-target checking pass.
The coverage receipt leaves **1325 collected cases in 130 files**, plus the shared
missing-dependency/version skips and platform matrix. Full Python/client completion
is not established.

Fresh-process result-checked 1000-operation probes retain elapsed/CPU time and whole/
incremental RSS. Batch insertion takes Rust 31–32 ms/46–47 MB versus C++ 219–247 ms/
117 MB; extracted/text query loops take Rust 71–73 ms/about 46 MB versus C++ 212–242 ms/
about 115–116 MB. These are development measurements with one engine thread, not final
release acceptance. Earlier individual outliers and the accepted uncached baseline
remain unchanged.

Evidence: `.context/artifacts/python-connection-20261025/python-connection-checkpoint.json`,
`statements-comparison-4.json`, `enum-pickle-current/`, isolated legacy-protocol receipts,
current signed SDK tests, `regression-matrix-final.json`, `remaining-runtime-coverage.json`,
`performance-comparison.json`, compiler terminal records and the complete source archive.
Broader argument/enum/packaging/version/platform and other selected client gates remain.

## All-types and Arrow producer checkpoint (2026-10-25)

The previously unexecuted all-types suite passes **205/205** on both engines;
timedelta and Arrow offsets pass another **342/342**. Dictionary/filter pushdown now
matches C++ with **155 passed, 2 existing skips, 2 existing xfails**, closing five Rust
failures. Polars filter pushdown/refeed/deprecation cases pass **72/72** after four
missing warning/alias cases are corrected. Deprecated Arrow aliases now warn before
using the actual native reader/table operations; preferred APIs do not warn.

The fallback Arrow scanner now returns its complete source schema and lets execution
project original column IDs. Previously it advertised projected output but returned
source-order columns, causing incorrect values and cross-type comparison errors.
The producer-aware path now sends actual projections and translated predicates into
Arrow Dataset/Scanner APIs, preserving datasets until scanner construction so fragment/
row-group pruning occurs before I/O. Polars lazy filters are applied before collect;
no unfiltered materialized-result cache is added.

A table-function filter-support callback leaves unsupported predicates in SQL,
including expression filters and unsupported Arrow layouts/types. Native comparison,
NULL, conjunction, nested-struct, IN, optional and NaN predicates translate to producer
expressions. Factory ownership now resides in cloned Arrow bind data, so filter rewrites
retain external inputs instead of losing their owner wrapper.

All **20 focused producer journeys** match C++, including projections, filtered
capsules/readers/datasets/scanners, owner lifetime and actual partition pruning. The
partition fixture includes an invalid Parquet file in an excluded partition; the query
succeeds by avoiding that file. This verifies source pruning, not just EXPLAIN text.
The nine signed Arrow tests and eight signed SDK metadata/type/profile tests pass.
Arrow/optimizer/function/Python all-target Clippy with warnings denied and whole-
workspace/all-target checks pass.

Twenty-three current development-wheel cohorts run **2673 cases: 2656 pass, 3 unchanged
failures, 14 skip/xfail**, with zero collection errors. Existing failures remain the two
held decimal reference discrepancies and shared Parquet timestamp-unit assertion.
The current cohorts leave **547 collected cases in 122 files**, plus the missing-
dependency/version/platform matrix. This is not full Python acceptance or final
core/client/shell/storage/WAL/release acceptance.

Coupled result-checked 1M-row/eight-column probes retain full and incremental RSS.
Table projection takes Rust 5.64–5.91 ms/257–258 MB versus C++ 5.49–6.37 ms/about 255 MB;
filtered-table scans are about 1.1 ms/192 MB versus C++ 1.1–1.2 ms/191 MB. Dataset filtering
records Rust 2.76 ms on the first run and 1.58 ms on repeat, versus C++ 1.61–1.63 ms;
whole peaks are about 132 MB versus 130–131 MB. The first-run slowdown remains visible.
These are development measurements, not final release acceptance. The accepted
uncached baseline and prior query outliers remain unchanged; no table cache is added.

Evidence: `.context/artifacts/python-all-types-20261025/python-all-types-checkpoint.json`,
current regression/coverage receipts, producer comparisons and partition fixture,
`performance-comparison.json`, signed wheel/tests, compiler records and source archive.
Further C API/native Arrow stream projection/filtering and broader producer/platform
matrices remain part of final verification.

## Filesystems, VARIANT and lifecycle checkpoint (2026-10-25)

The new five-file protocol cohort matches C++ with **65 passed, one existing httpfs
fixture skip and three existing VARIANT/Arrow xfails**, closing 40 Rust failures.
VARIANT host output now decodes native variant data recursively into the existing
scalar/nested converters for rows, NumPy and Pandas. Public VARIANT child metadata
rejects the internal storage structure. Registration errors include the user view name,
and connection APIs validate closure before relation construction/input binding.

Connection/module filesystem registration, removal, enumeration and lookup now use a
database-local native registry retaining fsspec backends. Native CSV/JSON readers,
Parquet positional reads, CSV/JSON/Parquet writers, globbing, COPY directory operations
and temporary-file moves route through streaming callbacks. Buffers and serialization
remain native; files are not converted to whole-file object-store snapshots. The
handle mutex is acquired before the GIL; seek/read or seek/write execute under one
attachment and one handle lock. Open handles retain their backend independently of
registry removal. Current external-access/disabled-filesystem checks precede callbacks.
General registered-filesystem database storage and broader backend/flag/error/platform
matrices still need explicit verification; the passing local `file://` ATTACH journey
does not establish arbitrary remote database storage.

All **85 focused VARIANT outcomes** match. Filesystem comparisons match **15/16**:
the C++ overwrite case fails after turning `memory://` into `memory:/tmp_x.csv`, while
Rust completes the overwrite. This is a **twelfth unaccepted reference discrepancy**;
raw outputs remain preserved. Do not emulate the malformed URI or call it accepted.

The streamed synthetic source generates bytes only on requested reads. A logical
200 MB/100M-row source returns a one-row prefix with 64 MB total bytes read across sniffing
and execution, demonstrating bounded reads. Initial small-source measurements exposed
a 32 MB CSV allocation/request for a 2 MB file and about 94 MB peak versus C++ 66 MB. Native
CSV reads now cap known-size, seekable, uncompressed UTF-8 buffers by remaining bytes;
compressed/transcoded and unknown-size sources retain their original handling.

Coupled repeats after the correction record Rust 64–71 MB/7.3–7.8 ms versus C++ 66–74 MB/
9.5–10.5 ms for the 2 MB source. For the 200 MB logical source, Rust is about 156 MB/
15–16 ms versus C++ 157–160 MB/17–18.5 ms; both read 64 MB. The original outlier and all
request/read counters are retained. These are development measurements, not final
release acceptance. The accepted uncached suite baseline and prior query outliers
remain intact; no result/table cache is added.

Twenty-four current development-wheel cohorts run **2742 cases: 2721 pass, 3 unchanged
failures, 18 skip/xfail**, with zero collection errors. Existing failures remain the two
held decimal reference cases and shared Parquet timestamp-unit assertion. Current
format and producer suites pass, including compressed/transcoded inputs and positional
read atomicity. All 23 signed CSV/JSON/SDK tests pass; three existing JSON timing probes
remain ignored. Warnings-denied all-target Clippy and workspace/all-target checking pass. The coverage receipt leaves **478 collected cases in 117 files**, plus
shared dependency/version skips and platform/package matrices. Full Python, other
selected clients and final core/shell/storage/WAL/release/performance gates remain open.

Evidence: `.context/artifacts/python-protocols-20261025/python-protocols-checkpoint.json`,
current regression/coverage receipts, VARIANT/filesystem comparisons, streamed-source
before/after measurements, signed native test logs/compiler records and source archive.

## Remaining gates

Complete relation APIs (including delete, remaining file formats/filesystems, mutation/type/metadata and
remaining expression overloads), remaining display/error semantics, broader result lifetimes,
remaining UDF overload/annotation/concurrency coverage, replacement scans,
complete native Pandas/NumPy inference, nested conversion and input error semantics,
complete Arrow registration/protocol/schema/error/pushdown
coverage, complete reader/capsule/concurrency behavior, remaining Polars semantics and Torch output,
filesystems, helper/function-catalog compatibility and remaining type/metadata/exception
semantics are still required. The API inventory marks only
partial implementation/evidence; no surface is declared fully complete yet.

All 275 upstream files and their parametrized cases still need full collection/running
with applicable dependencies; packaging/platform/version/lifetime and cross-engine
storage/WAL journeys remain open. Development-wheel correctness does not establish
release performance. Preserve the accepted uncached speed-memory baseline and report
individual query outliers in final acceptance. No table-cache retention was introduced.
The other selected language clients remain part of the goal.

Previous JSON evidence: `.context/artifacts/python-json-20261024/python-json-checkpoint.json`,
`json-comparison-final.json`, isolated subprocess receipts, `spark-rust-current/`,
the thirteen scoped `*-rust-current/` cohorts, signed `wheels-dev-10-signed/`, native
JSON tests, Clippy/workspace terminal records and all coupled performance experiments.

Previous profiling/package evidence: `.context/artifacts/python-profile-20261024/python-profile-checkpoint.json`,
`profile-comparison-current.json`, raw `probes-{cpp-1,rust-current}.json`, signed
`wheels-dev-6-signed/`, the thirteen `*-rust-current/` cohorts, typed tuple comparisons, four signed SDK tests and
compiler terminal records. Full collection/dependency receipts are under
`.context/artifacts/python-collection-20261024/`; collection is not test execution.

Previous nested/type evidence: `.context/artifacts/python-nested-20261024/python-nested-checkpoint.json`,
`types-comparison-final.json`, the twelve `*-rust-final/` cohort directories,
`type-tests.log`, signed `wheels-dev-9-signed/`, Clippy/workspace terminal records and
coupled UDF performance repeats. The source archive includes the uncommitted migration.

Previous map evidence: `.context/artifacts/python-map-20261024/python-map-checkpoint.json`,
`map-comparison-verified.json`, `isolated-{cpp,rust}-verified/`, `upstream-rust-final/`,
`{table-function,aggregates,parquet,csv,udf,dbapi,host-expanded,arrow,pandas-object,relation-expression}-rust-final/`,
`performance-comparison{,-optimized,-current}.json`, `prefix-memory-comparison.json`,
`clippy-final.jsonl`, `workspace-check-final.jsonl` and signed `wheels-dev-8-signed/`.
The source archive and SHA manifest include the still-uncommitted native-byte migration.

Previous aggregate evidence: `.context/artifacts/python-aggregates-20261024/python-aggregates-checkpoint.json`,
`{aggregation,parquet,arguments,csv,values,describe,update,udf,render,expression,joins-writes,pandas,reader,input,host,relations,foundation}-comparison-current.json`,
`{table-function,aggregates,parquet,csv,udf,dbapi,host-expanded,arrow,pandas-object,relation-expression}-rust-current/`,
signed `wheels-dev-6-signed/`, `clippy-3.jsonl` and `workspace-check-current.jsonl`.

Previous Parquet evidence: `.context/artifacts/python-parquet-20261024/python-parquet-checkpoint.json`,
`{parquet,arguments,csv,values,describe,update,udf,render,expression,joins-writes,pandas,reader,input,host,relations,foundation}-comparison-verified.json`,
`{parquet,csv,udf,dbapi,host-expanded,arrow,pandas-object,relation-expression}-rust-verified/`,
signed `wheels-dev-verified-signed/`, `function-tests.log` (37 signed tests, zero failures),
`clippy-verified.jsonl` and `workspace-check-verified.jsonl`.

Previous CSV evidence: `.context/artifacts/python-csv-20261024/python-csv-checkpoint.json`,
`{csv,values,describe,update,udf,render,expression,joins-writes,pandas,reader,input,host,relations,foundation}-comparison-current.json`,
`{csv,udf,dbapi,host-expanded,arrow,pandas-object,relation-expression}-rust-current/`,
signed `wheels-dev-verified-signed/`, `object-store-test-final.log`,
`clippy-verified.jsonl` and `workspace-check-verified.jsonl`.

Previous values evidence: `.context/artifacts/python-values-20261024/python-values-checkpoint.json`,
`{values,describe,update,udf,render,expression,joins-writes,pandas,reader,input,host,relations,foundation}-comparison-verified.json`,
`{udf,dbapi,host-expanded,arrow,pandas-object,relation-expression}-rust-final/`, signed
`wheels-dev-8-signed/`, `clippy-final.jsonl` and `workspace-check-final.jsonl`.

Previous UDF evidence: `.context/artifacts/python-udf-20261024/python-udf-checkpoint.json`,
`{udf,dbapi,host-expanded,arrow,pandas-object,relation-expression}-rust-optimized/`,
`{udf,render}-comparison-optimized.json`, signed `wheels-dev-optimized-signed/`,
`catalog-tests.log`, `clippy-final.jsonl` and `workspace-check-final.jsonl`.
The catalog regression suite passes 9/9 signed tests, including opt-in internal function
removal, rollback/commit and builtin type protection. Python/shell all-target Clippy with
warnings denied and the workspace/all-target compilation pass. These are development
checkpoint gates; full shell/corpus/client/release/performance gates remain open.

Previous expression evidence: `.context/artifacts/python-expression-20261024/python-expression-checkpoint.json`,
`comparison-{expression,joins-writes,pandas,reader,input,host,relations,foundation}-final.json`,
`{dbapi,host-expanded,arrow-upstream,pandas-object,expression-upstream}-rust-final/`,
`expression-upstream-cpp-3/`, signed `wheels-dev-final-signed/`,
`expression-clippy-final.jsonl` and workspace checks. Current failures are indexed in
each cohort's `remaining-gaps.json`; initial collection/error evidence is retained.

Earlier join/write evidence: `.context/artifacts/python-relations-20261024/python-relations-checkpoint.json`,
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
