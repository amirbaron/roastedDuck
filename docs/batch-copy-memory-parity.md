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

## First signed runtime counterexample

The source-4 SDK run passes injected-error cleanup/recovery and the nullable
Unicode CSV round trip, then stalls in the pressure COPY with a partial output
file. A bounded native thread sample shows the query waiting and all three
worker threads idle. The 180-second SDK watchdog will preserve the failed gate;
no shipping build or performance acceptance follows this run. Evidence:
`sdk-run-1.log`, `sdk-pressure-sample-1.txt` and the SDK terminal/verification.

Lifecycle review identifies a missing worker-finish handoff: frozen C++ COPY
Combine updates the memory manager's minimum batch index after submitting
remaining rows, then executes queued tasks. The new Rust candidate added
blocking but its old Combine path does not advance that minimum or wake
remaining blocked sinks. Correct that lifecycle instead of forcing one thread
or disabling the memory limit. Retain before/after regression evidence.

Fresh C++ controls also establish that the pressure COPY itself and order check
succeed at 96 MB, while its later EXCEPT ALL validation hits OOM at that budget.
The uneven-batch and injected-error controls pass. CSV's initial combined
control also hits OOM; stage-specific follow-up is needed to distinguish COPY
from CSV validation. Preserve the 96 MB COPY budget while giving complete-result
validation its own budget, as the measured before-probe already does.

## Worker-finish correction and calibrated validation

The first SDK watchdog expires at 180 seconds and records exit 124 after its
first two cases pass. Its signed source-4 image, thread sample and partial
pressure files remain preserved; the gate is failed. The worker-finish path
now advances the manager's minimum index after handing off final rows, flushes
queued work and wakes blocked tasks. NextBatch likewise hands off rows before
adjusting the manager minimum. Source-5 workspace/all-target Clippy passes.

The SDK pressure/CSV tests retain their 96 MB COPY budget, then explicitly set
512 MB for complete-result validation. The repeated C++ controls now pass all
four scenarios, including successful pressure COPY/order/full equality, uneven
batches, CSV equality/order and injected-error cleanup plus connection recovery.
The original low-budget validation failures are preserved as reference controls,
not reported as COPY failures. Evidence: `cpp-sdk-controls-2/report.json` and
`clippy-build-3-terminal.json`. Source 5 records 910 files/909 inputs; SDK build 2 is
active and its signed runtime outcome is still pending.

## Signed source-5 component acceptance checkpoint

SDK build 2 exits successfully; its immutable signed binary passes all four
regressions in 0.67 seconds, including the pressure COPY that deadlocked in
source 4. The 180-second watchdog does not fire. All 909 source hashes and strict
binary signatures verify before/after. All 25 signed row-layer unit tests also
pass, including buffer-managed collection round trips, strings/nested data,
layout and hash regressions. The C++ controls-2 receipt independently verifies
all four reference scenarios and its native signature.

Evidence: external `batch-copy-memory-20261027/{sdk-verification-2.json,
row-units-verification-1.json,cpp-sdk-controls-2/verification.json}`. The first
failed candidate remains retained separately; no source-4 gate is relabeled.

Shipping CLI/C API/SQL runner images are rebuilding from source 5 for independent
COPY output and repeated timing/RSS on the same before fixture. These passing
component tests establish runtime progress, not a measured memory improvement
or final performance acceptance. Broader CSV/Parquet SQL, C API/ADBC, selected
clients, full corpus/shell, persistence/WAL and final benchmark gates remain.

## First shipping measurements: performance acceptance rejected

The source-5 shipping CLI/library/runner are signed and immutable; 547 C API
exports and zero ADBC test hooks are preserved. All 36 coupled samples complete
against C++ and the preserved old Rust image, with three trials per engine at
each configuration and rotated engine order. Each of 33 successful output files
is read by the opposite engine and every one of its two million payloads and
physical row IDs is checked. All 909 source inputs/library signatures/input hashes remain
stable. Evidence: `native-images-1.json`, `after-probe-1/verification.json`.

| Configuration | C++ median ms / MB | Old Rust median ms / MB | Candidate median ms / MB |
|---|---:|---:|---:|
| 1 thread, 256 MB | 292.2 / 150.6 | 391.9 / 162.8 | 393.1 / 162.6 |
| 14 threads, 256 MB | 235.8 / 459.3 | 82.9 / 665.5 | 370.1 / 439.9 |
| 14 threads, 512 MB | 147.2 / 698.9 | 81.4 / 690.2 | 310.2 / 629.7 |
| 14 threads, 256 MB,skew | OOM in 3/3 | 106.4 / 907.5 | 446.4 / 637.9 |

Memory decreases 33.9% at 14 threads/256 MB against old Rust, while time increases
4.47 times. At 512 MB the candidate saves 8.8% memory for 3.81 times the runtime;
the skewed successful copies save 29.7% memory for 4.20 times the runtime. Those
large regressions are not accepted or hidden in an aggregate. Error timings
and RSS are retained for C++ skew but no successful speedup ratio is computed
against its failed copies. Final TPC-H/ClickBench acceptance remains separate.

A bounded repeated-COPY native profile shows many idle worker stacks while
encoding/managed scans execute. Review identifies another difference from
frozen C++ AddLocalBatch: C++ wakes blocked helper tasks and executes prepare/
flush locally only when no helper was woken. The current Rust candidate wakes
helpers and always performs preparation on the handing-off worker, delaying
minimum-batch progress. Restore the C++ conditional handoff, retain the memory
accounting and buffer management, then reverify correctness and repeat the
same coupled controls. Do not raise memory to recover the old speed. Evidence:
`profile-1/{sample.txt,ready.json,terminal.json}`. This is a diagnosed candidate
change, not a proven performance fix.

The conditional C++ helper handoff is now implemented without changing memory
reservations or raw/prepared accounting. Source-6 workspace/all-target Clippy
passes; SDK build 3 is active for the same four regressions. New build/freeze/
measurement stages use explicit stage/source arguments and preserve all
source-5 images/results. This handoff has not yet passed runtime or performance
verification. No rejected source-5 performance result is relabeled as accepted.

The signed source-6 SDK also passes all four regressions (0.66 seconds). A final
C++ lifecycle comparison identifies the paired helper-resume requirement:
FixedBatchCopyLocalState retains PROCESSING_TASKS across a blocked sink and
executes queued work when resumed even if its batch has meanwhile become the
minimum. The Rust candidate currently recomputes assistance only from memory
pressure/minimum status and retains no processing phase. Complete that state
transition before the next shipping build, preserving the accepted component
evidence and all failed performance controls. Runtime/performance improvement
remains unproven.

The resumed-task processing phase is now retained in CopyLocal across blocked
sink calls. Woken helpers drain queued preparation/flush work before resuming
input, including when their batch becomes the minimum, matching the frozen
C++ PROCESSING_TASKS/SINKING_DATA lifecycle. Memory reservations/accounting are
unchanged. Source-7 Clippy reports only a collapsible-if style issue; source 8
collapses that condition and full workspace/all-target Clippy passes. Source-8
SDK build 4 is active for the same four regressions. Source 6's passing SDK and
source 5's rejected shipping performance evidence remain preserved separately.
This lifecycle revision has no runtime/performance acceptance yet.

## Signed complete helper-lifecycle component checkpoint

The source-8 release SDK build exits successfully and its immutable signed
binary passes all four regressions in 0.66 seconds, including memory-pressure
COPY, uneven/final batches, CSV and error cleanup/recovery. The watchdog does
not fire, and all 909 source hashes/signatures verify stable. Evidence:
`sdk-verification-4.json`, `sdk-run-4.log`. The second shipping build is active
from source 8 with the explicit stage-2 source guard. Repeat the same coupled
controls only after successful completion and signing; no performance
improvement or tradeoff acceptance is claimed by this component result.
