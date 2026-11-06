# Query progress publication and shell activation

The real progress display probe in `shell-argv-20261025/progress-journey-2.jsonl`
verifies six fixed-frame/cancel/reuse journeys, but consistently records an extra
Rust progress frame for the following SELECT42. C++ prints one frame for the long
query; Rust prints two across long and short queries. This is an open activation
semantic difference, not dismissed as harmless timing nondeterminism.

Audit the public C API's pending materialized/streaming task steps and query-progress
percentage/row counters before changing core publication or shell activation. Preserve
all steps and image identities, and verify actual scalar results and connection reuse.
Use the immutable signed Rust library3 only as a diagnostic prior image: its active_query
source SHA still equals current source (18648fbac17db759263b210ac33b0b82c054e83008d9e0cf913a90c6eab4c623).
The frozen C++ library and both signatures are currently verified. This is not a fresh
current-source native library acceptance run.

C++ initializes its internal bar progress to unknown while public client progress
restarts at0. Unsupported pipeline updates preserve internal unknown state; known
updates publish row counters even at0percent. Rust currently reuses public STARTED0
and updates counters only if percentage increases. Measure those boundaries rather
than adding a fixed shell delay. If core SDK publication changes, expand validation to
signed SDK/C API images and fresh Python bindings; do not reuse wheel4 as current-core
evidence. Keep all workloads serial/nice10/j1 and preserve the lower-memory policy.

## Public ABI baseline

The maintained pending-task driver runs six real materialized/streaming journeys on
both signed libraries, checks actual scalar data chunks and connection reuse, and
retains every unmodified task/progress snapshot. Both modes' result streaming flags
match. For the constant query, C++ switches public STARTED0 to unknown-1 after its
first task; Rust keeps0through pending execution. For empty/range aggregates, C++
publishes positive source totals at0percent while Rust leaves those counters0.
Evidence: `progress-capi-{cpp,rust}-2.json` under the shell artifact task.

Mirror the distinct internal unknown/public restarted progress states. Publish row
counters on known0percent updates as well as increases, preserve the highest known
percentage across unsupported pipeline updates, and expose completed executor progress
as an empty pipeline set. C++'s0/0final ratio does not increase the last percentage.
Do not add a shell sleep or retain table blocks. Stream-close progress/lifetime details
in the trace remain separate verification boundaries, not silently accepted differences.

## Fresh signed ABI checkpoint and remaining accounting

The first fresh library (progress-library-1, SHA
d0b648afc257627bec71c9bc37d12764a7ad36aba69cdb5f6ef9663b181acfb3) fixes constant
unknown progress and known0percent counter publication. Two signed SDK tests pass in
both result modes. All12partial/exhausted public C API journeys preserve scalar data,
streaming flags and reuse, with dladdr verifying the actual selected function image.

The fresh trace still reports totals one below C++ for aggregates. Source explains
that contribution: C++ includes the uninitialized root collector pull pipeline
(collector estimated cardinality0 clamped to1); Rust builds only its child push
pipelines. Include that root progress contribution without scheduling another task.
Use the root's cardinality metadata, not a query-specific displayed constant.

CPPpartial stream close retains its last progress measurement until a new query starts;
fully exhausted streams reset to unknown. Rust currently resets both on close. Preserve
only the already held scalar progress metadata after explicit close, while releasing
the query's resources. Reset it at initial cleanup before preparation/new execution.
Do not retain a result/table cache to reproduce this public observation. Broader stream
transaction/ownership semantics remain separate client/storage verification gates.
Evidence: `progress-capi-{cpp,rust}-3-{partial,exhaust}.json`; earlier images remain.

## Signed corrected progress publication

Library2 (SHA b958e90af7ee0b154d10163c8949191a3ee9fee4d190232b55ebbf5ef58b6c4c)
now matches all12public progress lifecycle paths across materialized/streaming and
partial/exhausted results. Actual scalar values, streaming flags, row counters,
unknown/started/closed/prepared transitions and connection reuse match. dladdr verifies
the selected Rust/C++ function image for every process. All547DuckDB exports are
unchanged. Three signed SDK regressions pass, with current all-target workspace checks
and warnings-denied Clippy passing.

The comparison coalesces only repeated identical task observations; raw snapshots
and task counts are retained separately. CPPuses one/two additional scheduler steps
in these cases. This is not a claim that the raw scheduler traces are byte-identical.
`progress-verification-1.json` indexes evidence and scope under the SSD shell task.

The shell is rebuilding for actual progress activation verification. Fresh shell,
Python, broad C API/core/storage and final performance gates remain pending after
these core changes. Prior CLI9/wheel4/full-corpus3 evidence is not relabeled as current.
Only scalar progress metadata survives partial result close; query resources are
released, and no table/result cache or fixed progress delay is added.

## Default-thread terminal follow-up

CLI10's default-thread progress run retains correct component bytes and cancellation/
reuse where a frame appears, but the fixed one-second interval misses frames on either
engine in SQL-component cases. The partial run is preserved as `progress-journey-3`;
it does not pass the six-case gate. Replace that timing assumption with a bounded wait
for the actual component marker before cancelling, keeping raw timing/count differences.

Raw streams also confirm a deterministic leading clear from Rust's SET setup, absent
on C++. The shell creates a ProgressDisplay for a direct statement with no underlying
progress bar. Require the backend's restarted/known progress state before creating
that display; pending direct statements remain unknown, while running plans restart
at0before tasks. Do not insert a fixed delay or force engine threads to1.

## Progress capability fixture correction

The missing default-thread frames in the range-based visibility probe are explained
by source capability: frozen C++ range has no progress callback, and Rust's generic
table-function source uses invalid progress by default. An initial uninitialized
pipeline can briefly report0percent; those setup frames are scheduler-dependent.
Do not require ongoing progress from that unsupported source or label its missing
frames an engine regression. Preserve all earlier raw runs and the capability audit
`progress-range-fixture-audit-1.json`; none is whole progress-visibility acceptance.

Use a100,000-row stored table (one BIGINT column, about0.8MB base data) joined with
itself under a scalar aggregate, so table scans support progress and the build side
stays bounded. Configure custom components after table setup, wait for actual progress,
then interrupt and verify query reuse. This keeps normal engine threads and tests real
supported progress without a large intermediate dataset or a synthetic engine callback.

## Supported progress display and current regressions

Signed CLI11 (SHA ded15853775e05c0208e88ace90b276f41728c919aa31d037267ab426fdccedf)
passes all six stored-table progress journeys: fixed literal/SQL component bytes,
hide/padding, actual display readiness, cancellation status and SELECT42reuse match.
Neither engine times out waiting for supported progress. Raw redraw counts differ
(three CPPframes versus three/four Rustframes), and only two whole streams are exact;
no full-stream parity claim replaces those raw observations. The leading SET clear
is fixed. `progress-display-verification-1.json` and `progress-journey-5.jsonl` retain
all bytes/counts. Previous range-based runs remain an unsupported-source fixture audit.

CLI11passes392upstream shell tests/3existing skips and all seven existing byte-guard
case/verdict sets remain unchanged. The new bounded output-wait mechanism has a real
negative control: identical CPPprocesses with an impossible marker correctly fail the
readiness gate. Full768case/46PTYdifferential is running; fresh Python, broad native
C API/core/storageWAL and final performance/client acceptance remain pending.
