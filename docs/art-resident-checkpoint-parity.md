# Native WAL recovery and resident-only ART checkpoints

The full port goal remains active. This checkpoint closes the observed native-index
recovery and cold-checkpoint memory gaps without adding a table cache or raising
memory limits. It follows the earlier [ART persistence work](art-persistence-parity.md).

## Resulting behavior

ALTER ADD PRIMARY KEY replays its native allocator buffers instead of rebuilding the
index from table rows. Its catalog/storage change uses the normal binder and undo
hooks, while the native index stays outside the active list until the creating WAL
transaction commits. Entries without usable native metadata keep the rebuild path.

The WAL reader writes one allocator buffer at a time and flushes all deferred writes
before publishing the rewritten disk locations. It no longer retains a second whole
index as nested byte vectors. The metadata loader also accepts persisted constraint
order when ALTER or rename preserves index names different from generated ordinals.
It consumes each constraint description before considering the next constraint.

Checkpoints of indexes already persisted in the deprecated layout visit resident
subtrees, matching C++'s InMemoryRef/LoadedFromStorage walk. Untouched disk subtrees
remain unloaded. Newly changed resident gates still convert; a cold modern-to-old
format change still traverses and converts the complete tree. No extra residency or
cache is introduced.

Before these changes, C++ recovered a 100,000-row ADD PRIMARY KEY WAL at 1 MB while
Rust rebuilt the index and failed. An additional cold reopen after checkpoint exposed
an index-name mismatch and the same rebuild. Both stages now pass in both engines.
A separate two-million-row plain/expression-index CHECKPOINT at 1 MB passed in C++
but made Rust load the tree, run out of memory, and invalidate the checkpoint attempt.
Both engines now pass those cases with `temp_directory=NULL`.

## Current signed-artifact verification

Evidence is `art-resident-checkpoint-20261029/source-1/manifest.json`: 969 overlay
entries and 967 guarded build inputs. Each native build parent actually exited zero
before images were frozen and signed. Hashes/signatures are checked before and after
execution, and raw failures from the earlier attempts remain preserved.

| Gate | Result |
|---|---|
| Workspace/all-target Clippy, warnings denied | Pass |
| Signed focused SDK debug/release | Nine pass in each profile, zero ignored; cold expression test strengthened to two million rows at the unchanged 1 MB limit |
| Signed storage/catalog/execution tests | 141 pass, zero ignored, including 23 index tests |
| Original transactional-WAL and legacy SQL controls | Both pass |
| Native cold ADD PRIMARY KEY recovery and checkpoint reopen | Both stages pass at 1 MB in C++ and Rust |
| Native two-million-row plain and expression-index recovery/CHECKPOINT | All four engine/case combinations pass at 1 MB, spill disabled |
| Full SQL corpus | 4,870 files: 4,781 pass, 74 exact C++ skips, 15 existing documented exceptions; zero crashes/timeouts/verdict regressions/changed skip reasons |
| Fixture and ratchet integrity | All 1,522 fixture hashes stable; expectations/ratchet unchanged |
| Full C API suite | 30 files, 155 cases, 728,156 assertions pass; actual loaded current library verified |
| Cross-engine recovery | 52 journeys pass: 24 expression, four transactional/string-key, 24 primary-key; plain/GCM/CTR, clean/torn tails, receiver writes/checkpoint and writer readback |
| Full workspace unit/integration | 1,001 tests across 141 signed executables pass; seven existing diagnostics/probes ignored and listed in the report |
| Shell | C++ and Rust each 392 pass / three shared skips; 768 differential cases including 46 PTY sessions have zero unexpected differences |
| Python/Go/Swift, native/installed ADBC, final TPC-H/ClickBench | Final refresh still in progress |

Native SHA256 values:

- Library: `01aaeb24d068580f7755656062ba7a3e8a31bbbdfd4a1c5933379a59ecb0a702`
- Shell: `d53e9768c456ff13d0327c0c9960e485dc1ff9f40af9223cf539c24a4a90a5b7`
- SQL runner: `5942f2cfd1c6f970e10350bf6d8afb59f33f56c965f87fedc4470c01344ada5d`

The shipping library has all 547 C API exports and no ADBC test hooks. The reference
is the frozen C++ 1.5.6 source build (`994623d0d325e553783c6f27ef6436837d4601faec64bb70a721b2f1fcdab61b`), retaining its literal `v0.0.1` version metadata.

## Coupled recovery measurements and remaining outlier

Three paired/interleaved repetitions use the same preserved C++ two-million-row WAL
inputs, 14 threads, a 256 MB limit, and no spill directory. Every sample checks row
counts/sums and unique constraints. Times below are medians; individual samples and
min/max ranges remain in `measured-recovery-1/verification.json`. Peak RSS includes
the interpreter/runtime. Native close is timed separately from post-run hashing and
signature verification; image hashes stream through bounded buffers.

| Case / engine | Attach ms | Native close ms | Recovery + validation + close ms | Peak RSS MiB |
|---|---:|---:|---:|---:|
| Plain / C++ | 10.492 | 4.352 | 16.560 | 59.59 |
| Plain / earlier Rust | 6.907 | 82.027 | 91.841 | 75.45 |
| Plain / current Rust | 6.883 | 17.668 | 26.391 | 61.03 |
| Expression / C++ | 10.198 | 4.425 | 16.273 | 59.38 |
| Expression / earlier Rust | 6.750 | 83.402 | 91.882 | 75.53 |
| Expression / current Rust | 6.866 | 18.276 | 27.808 | 61.81 |

The resident-only traversal reduces Rust close time by about 78% and peak RSS by
about 19%. Rust still has a roughly 13–14 ms close-time outlier and slightly higher
process RSS in these recovery workloads; faster attach does not conceal that cost.
These component measurements do not establish final TPC-H/ClickBench acceptance.
The earlier `wal-recovery-memory-20261029/before-1` timing/RSS run included whole-image
hashing and signatures in its close interval and is retained as a confounded diagnostic,
not used as performance acceptance. Its actual SQL memory-limit failures remain valid.

## Exceptions and evidence

The shared recovered deleted-key reference limitation and C++ premature-close fault
are explicit in [divergences](../conformance/divergences.md). Their failed independent
correctness diagnostics remain separate from passing paired controls. Current Rust
retains the recoverable WAL when unbound expression mutations cannot yet be bound.

Primary evidence root:
`/Volumes/amirs ssd/ai context/conductor/duckdb-rust/ottawa-50198a764d3b1fdc/art-resident-checkpoint-20261029/`.
Preceding failures and native ALTER/streaming/order controls remain under
`wal-recovery-memory-20261029/`; early expression/legacy controls remain under
`art-persistence-parity-20261029/`. Source overlays, manifests, signed images, original
input files, process terminals, full logs, and timed samples are preserved there.

Final acceptance still requires the requested same-source unit, shell, C API/ADBC,
Python, Go, Swift, persistence/WAL and coupled TPC-H/ClickBench gates. Preserve the
owner's accepted 15–20% longer / 60–70% lower-memory baseline and documented COPY
tradeoff; no table cache, independent exact performance parity, outperformance or
additional language clients are required.
