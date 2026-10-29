# Native ART persistence and deferred WAL replay

The port goal remains active. This component targets DuckDB 1.5.6's native ART
storage and replay behavior, including supported v0.9.1 database files. It adds no
table cache and raises no test memory budgets.

## Change

Expression indexes bind from persisted allocator metadata instead of rescanning the
table. Mutations replayed before the expressions can be bound retain the index's
physical key columns and row IDs, then apply insert/delete events in order to a
freshly restored tree. Retained copy-on-write vectors survive the producer's next
mutation or release. Failed binding keeps the original storage and replay events
available for retry.

CREATE INDEX WAL entries restore native allocator buffers for plain and expression
indexes. Deferred block writes must flush successfully before rewritten pointers
become visible; a flush failure leaves the original WAL representation retryable.
The pre-v0.10 root container's six allocator pointers and old allocator records are
converted to the same lazy metadata used for modern files. The upstream v0.9.1
compressed fixture is retained unchanged in the SDK tests.

An automatic checkpoint without a binding connection defers while unbound expression
indexes have pending replay events. The existing checkpoint and nonempty WAL remain
recoverable; explicit CHECKPOINT binds and applies the events first. The commit
optimization that omits WAL writes is ineligible while such events remain. The
catalog serializer also refuses to publish an unchanged tree with pending events.

## Verification

Source 14 has 969 overlay entries and 967 guarded build inputs. All checks below
use source guards and immutable strictly signed binaries, with actual build parent
exit status verified before signing or execution.

| Gate | Result |
|---|---|
| Workspace/all-target Clippy, warnings denied | Pass |
| Signed focused SDK debug and release | 8 pass in each profile; zero ignored |
| Signed storage/catalog/execution tests | 138 pass, zero ignored, including 20 index tests |
| Original transactional-WAL and legacy SQL smoke tests | Both pass |
| Affected SQL corpus | 796 files: 777 pass, 19 exact C++ skips; zero failures/crashes/timeouts/regressions or changed skip reasons |
| Full C API suite | 30 files, 155 cases, 820,026 assertions pass; actual selected library verified |
| Cross-engine numeric expression-index journeys | 24 pass across plain/GCM/CTR, persisted/CREATE INDEX WAL and clean/torn tails |
| Additional cross-engine journeys | Four pass: transactional index/PK/drop and multi-chunk NUL/Unicode string expression keys, both directions |
| Cold 100,000-row expression-index checkpoint at 1 MB, no spill | C++ and Rust both pass; old Rust failed |

The first signed native affected run exposed transactional CREATE INDEX WAL recovery
using normal duplicate checks and publishing an index before its creating replay
transaction committed. The failed run was stopped and preserved. Recovery now uses
C++'s dedicated WAL append semantics and stages the index until commit. Both the new
SDK regression and the original upstream SQL test pass after the correction.

Current native image SHA256 values:

- Library: `52098b287d43ac6132d0e743bc6bca1bcc15b0007065676e0e662d6e027af6cf`
- Shell: `a5c4a6dda79bcd1f61970a710fc17a00ee1a078dd4f6744feb7396f77b9d3170`
- SQL runner: `4a33665268951a1f3c8b28df674e482ecf4c82dfb5c90c3a40096fca279e6d7e`

The library exposes 547 C API functions and no ADBC test hooks. The reference is the
frozen C++ 1.5.6 source build, SHA256
`994623d0d325e553783c6f27ef6436837d4601faec64bb70a721b2f1fcdab61b`.
Its literal version metadata remains `v0.0.1`; it has not been relabeled.

The SDK checks a 100,000-row cold expression-index checkpoint with a 1 MB limit and
`temp_directory=NULL`, plain CREATE INDEX WAL restore under the same limit, native
legacy ART buffers unresident before lookup, nonleading physical key columns,
insert/delete/update/rollback replay, unique-key checks, deleted-key reuse, and two
successive unbound closes for both persisted and WAL-created expression indexes.
Lower-level tests cover ordered replay after source-vector mutation/release, failed
binding retry, and deferred write failure/retry.

## Reference controls and limitations

Before the fix, the cold persisted expression-index checkpoint passed in C++ at 1 MB
but Rust failed a 256 KiB allocation at 768 KiB used. Table summaries alone did not expose
that full-table rebuild.

The automatic-close control exposed a separate shared C++/old-Rust fault: after
CREATE INDEX in WAL and later key mutations, closing before binding checkpointed
correct table rows with stale expression-index keys. C++ readback missed new/updated
keys, accepted duplicate unique keys and still rejected the freed old key. An
explicit checkpoint control passed in both engines. The preserved `before-4` inputs
include post-close files before readback. Rust's new deferral protects recovery;
reproducing the reference's wrong results is not an acceptance requirement.

A separate diagnostic creates a unique index and deletes an existing base row
(`v=42`) in the same transaction, then recovers its WAL. The table multiset correctly
excludes that row, but both the frozen C++ engine and Rust reject reinserting 42 with
the same unique-constraint error (type 18). This is a **shared reference limitation**,
not a passing correctness control or a Rust-only divergence. Both failed attempts,
original database/WAL inputs and error logs remain in `diagnostics-1/`; no failed
expectation or ratchet was rewritten. See `conformance/divergences.md`.

Read-only recovery of native CREATE INDEX WAL buffers is also rejected by both
engines with IO error type 28. Their raw OS/error text is retained separately. The
canonical primary-key read-only WAL replay test passes in the 796-file cohort.

## Evidence and remaining gates

Evidence root:
`.context/artifacts/art-persistence-parity-20261029/` resolves to
`/Volumes/amirs ssd/ai context/conductor/duckdb-rust/ottawa-50198a764d3b1fdc/art-persistence-parity-20261029/`.
Source overlays, hashes, compiler failures, signed binaries, process terminals and
logs remain there. The compiler failures are not represented as test passes.

Final same-image acceptance still requires the full SQL corpus, shell, C API/ADBC,
Python, Go and Swift, cross-engine plain/encrypted checkpoint/WAL recovery and
coupled TPC-H/ClickBench speed/memory measurements with per-query outliers. The
owner's accepted lower-memory baseline and memory-bound COPY tradeoff remain in
force; no exact speed/memory parity or additional language clients are required.
