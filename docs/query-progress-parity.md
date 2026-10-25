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
