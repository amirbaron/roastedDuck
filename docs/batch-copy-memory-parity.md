# Ordered COPY batch memory

The live work queue's bulk-load item remains real: Rust `copy_to_file.rs` buffers
raw batches as `Vec<DataChunk>` without `BatchMemoryManager`; frozen C++
`physical_batch_copy_to_file.cpp` tracks raw/prepared batches and caps workers
from its memory reservation. Its raw data uses `ColumnDataCollection`. The Rust
manager already exists for batch inserts, but COPY does not use it. Passing
SQL memory-limit tests checks output content/order, not measured peak RSS.

## Measured before controls (2026-10-27)

Separate signed native C API processes use one identical immutable two-million-
row Parquet input, with integer IDs and 128-byte deterministic strings. COPY
output is read back for exact count, ID sum, string-length sum, tuple checksum
and physical row order. Each child loads exactly its selected engine image.
All 905 source inputs/library hashes and signatures verify stable; the input file is
hashed outside measured children before every sample. Timing/peak RSS are
snapshotted immediately after COPY. Validation then uses a separate 1 GB budget.

An initial 14-thread C++ validation aggregate exceeded the original 256 MB budget
after COPY had successfully created its file; that harness failure is retained
in `before-probe-1`. It is not a COPY failure. The corrected probe stores COPY
measurements before raising the validation budget (`before-probe-2`).

Three repeated samples per engine at 14 threads/256 MB reverse engine order on
the second trial. Medians for regular COPY are:

| Engine | COPY wall time | Whole-process peak RSS | Outcomes |
|---|---:|---:|---|
| C++ | 246.3 ms | 426.0 MB | all three succeed |
| Rust | 93.9 ms | 666.1 MB | all three succeed |

Rust is faster here while using 56.4% more median memory. All six regular output
checks match literally and ordering is preserved. A separate skewed output
expands the first 131,072 strings eightfold: C++ returns OOM in all three runs;
Rust succeeds in all three, with 905.3 MB median peak RSS. Its successful outputs
pass validation. Failure durations/memory are retained as error evidence, not
used as successful speedup comparisons. These are bounded COPY diagnostics,
not final TPC-H/ClickBench acceptance. Initial one-sample controls at one thread and
14 threads/512 MB are also retained, including both timing and memory.

Evidence is on the external SSD under
`batch-copy-memory-20261027/{before-manifest.json,before-probe-2/verification.json,
before-repeat-1/verification.json}`. The signed Rust library is the verified
nested-hash source4 artifact, SHA-256
`889c74d3bb17a0615a6898eea1a8457cbdf0b9dee47bf759290865b14e93a1ad`.
No implementation or closure is claimed by these before controls.

## Required implementation and verification

Port the C++ reservation/accounting/throttling lifecycle, including 4 MiB per input
column per worker, the reservation-derived worker cap, raw/prepared ownership
transfer through repartition/prepare/flush, assistance before blocking, lowest
batch forward progress, task wakeups and zero unflushed accounting at finalize.
Use the existing temporary memory manager and buffer-managed collection facilities
where the C++ contract does. Preserve ordered output and error/cancellation
behavior; avoid serializing all copies to hide the gap. Do not copy table-cache
residency or change the accepted benchmark baseline.

Add meaningful ordered/uneven-batch/memory-pressure/finalization regressions,
then verify signed SDK/public output and existing batched Parquet SQL tests.
Repeat coupled timing/RSS on the same frozen input, retaining each sample and
individual skewed behavior; evaluate memory improvement and runtime cost
together. Refresh broader final artifact gates after remaining core changes.

## First implementation component stage

The working COPY sink now uses the existing BatchMemoryManager reservation, a
reservation-derived worker cap, raw/prepared charge transfer, ordered-flush
release, minimum-index progress and zero-at-finalize check. Blocking rechecks
available memory under the callback lock after helping queued tasks, preventing
a flush-before-registration wakeup loss. Raw batches now use buffer-managed
ColumnDataCollection with partition 0 eviction priority. Repartition scans and
appends one chunk at a time and preserves C++'s charge-transfer lifecycle.

A replayable CopyBatch interface lets format writers scan managed rows without
introducing a function/row dependency cycle (row already depends on function).
CSV encodes one scanned chunk at a time. Parquet replays selected columns for
analyze, prepare and write phases instead of materializing all flat heap chunks
at once. A scan pins one raw chunk's blocks and copies selected columns; it does
not claim per-column block-level pin isolation. The ordinary non-batched path
and table-cache policy are unchanged.

Workspace/all-target checking passes after correcting one private-module import;
final workspace/all-target Clippy with warnings denied also passes after a
bit-width style correction. Source 4 has 910 overlay files/909 build inputs. The first
three snapshots and failed check/style logs remain preserved; source 4 is the
current candidate. Four SDK regressions cover memory-pressure ordered Parquet,
uneven/final batches, nullable Unicode CSV and worker-error cleanup/recovery.
Their release build is active and runtime verification remains pending. No
memory reduction, runtime acceptance or completed manager port is claimed yet.

Evidence: external `batch-copy-memory-20261027/{source-4/manifest.json,
check-build-2-terminal.json,clippy-build-2-terminal.json,sdk-build-1-manifest.json}`.
Next: sign/run the completed SDK binary with its watchdog, fix any failures,
then build/sign shipping images and repeat the frozen coupled timing/RSS
controls plus independent C++/Rust output, CSV/Parquet SQL and broader gates.
