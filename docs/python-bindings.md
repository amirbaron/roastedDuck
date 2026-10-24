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

## Remaining gates

Complete relation APIs (including delete, remaining file formats/filesystems, mutation/type/metadata and
remaining expression overloads), remaining display/error semantics, broader result lifetimes,
remaining UDF overload/annotation/concurrency coverage, replacement scans,
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

Current nested/type evidence: `.context/artifacts/python-nested-20261024/python-nested-checkpoint.json`,
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
