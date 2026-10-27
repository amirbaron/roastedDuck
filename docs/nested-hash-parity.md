# Nested vector hash parity

A fresh 2026-10-27 audit confirms the work queue's VARIANT hash gap on the signed
encryption-stage Python image. All 30 separate-process expression controls differ
from pinned C++ 1.5.6, including null, booleans, integer widths, floating point,
strings, dates, lists and objects. Values/type labels agree in these controls;
the public hash result differs. Evidence is external
`nested-hash-parity-20261027/{before-manifest.json,variant-hash-comparison-1.json}`.
This is an open correctness gap, not an accepted performance exception.

The frozen C++ vector-hash source dispatches STRUCT fields through recursive
CombineHash. Its LIST combine path folds each element's standalone hash into the
existing seed, leaving null/empty lists unchanged. ARRAY combines each child hash
into the existing seed as well. Rust currently hashes nested fields independently
and combines the reduced hash once, including empty-list hashes. Combining hashes
is not associative, so those implementations differ for nested fields and
multi-column hashing, including VARIANT's physical STRUCT layout.

Port these existing C++ semantics across constant, flat, selected and cached
representations without changing the preserved cached-dictionary special case.
Add exact C++ controls for nested STRUCT/LIST/ARRAY, null/empty fields and multi-key
hashing; cover VARIANT values before/after checkpoint and encrypted/unencrypted
Parquet, flat/dictionary vectors and grouping/join correctness. Verify signed
component tests, public oracle results and affected SQL/hash-dependent storage
journeys before refreshing all final release gates. No implementation or closure
is claimed by this before-fix contract.

## First implementation component checkpoint

The working row layer now recursively combines STRUCT children and folds each
LIST/ARRAY child's standalone hash into the running seed, preserving the seed
for null/empty sequences. The constant and general fallback combine paths use
these semantics too. Scalar fast paths and the existing cached-storage-dictionary
special case are unchanged; no cache-residency policy changed.

Workspace/all-target checking passes. Clippy with warnings denied passes after
formatting separators in the new oracle literals; the first style-only rejection
is preserved. All 25 immutable signed row-layer unit tests pass, including exact
C++ controls for null/empty/populated lists/arrays and nested structs across
constant, flat, dictionary-selected and selected-row representations.

A release SDK test is building against 48 independently captured C++ controls:
30 VARIANT and 18 nested collection/struct/map/array cases, with standalone and
multi-argument hashes for constants, stored rows and filtered selections. SDK
execution, signed shipping/public re-probes, hash-dependent storage/grouping/joins,
full corpus/client/shell and final speed/RSS acceptance remain pending.

Evidence: external `nested-hash-parity-20261027/{source-2/manifest.json,
clippy-build-2-terminal.json,row-units-verification-1.json,oracle-1.json,
combine-oracle-1.json}`. Source 2 records 906 overlay files/905 build inputs. Nine
Python bytecode cache files from the previous overlay are deliberately excluded;
the newly changed tracked row/hash source and new SDK test are included. The
source-1 checkpoint differs only in oracle-literal formatting and is retained.

## SDK counterexample retained

The first signed SDK execution fails at JSON `'{}'::JSON::VARIANT`: actual
standalone hash 471934738805923757 versus C++ 17009719232678247413. Earlier
controls complete before that assertion, but this does not prove the whole set.
The source-2 SDK failure is retained in `sdk-run-1.log` and
`sdk-verification-1.json`; no shipping build or closure is authorized by that
failed component gate. A diagnostic source-3 SDK build now accumulates all
control mismatches rather than stopping at the first.

The frozen C++ `to_variant_fwd.hpp::WriteContainerData` always advances the BLOB
offset by both the length-varint width and children-offset-varint width, even
when length is zero and it writes only the length. C++'s inline string buffer
is zero-initialized. Rust's ordinary typed VARIANT converter already reserves
that space, but `json_to_variant.rs::VariantBuilder::container` currently omits
it for empty JSON containers. Check and correct that physical representation
after the diagnostic run, retaining safe initialization of unused bytes. Recheck
all original controls and add empty/nested JSON-container coverage; do not waive
JSON-derived values from the hash requirement.

## Complete diagnostic and JSON layout correction

The signed source-3 diagnostic completes all 48 controls: the only mismatching
expression is the empty JSON object, in both hash columns for constant/stored/
selected representations. All other 47 controls match. The full mismatch list
is retained in `sdk-run-2.log`; the component gate remains failed there.

The JSON converter now reserves a safely zero-initialized offset-width slot for
empty containers. Seven additional empty/nested JSON inputs are captured from
C++: each is stable in five independent connection trials, both constant and
stored-row hashing. The SDK test now covers all 55 controls. Source4 workspace/
all-target Clippy passes and its release SDK is building. This stage has not yet
passed runtime verification. Evidence: `oracle-json-extra-1.json`,
`source-4/manifest.json`, `clippy-build-4-terminal.json`.

The native storage journey is calibrated against C++ alone for all four plain/
encrypted checkpoint/nonempty-WAL controls, including receiver mutation and
readback, hash indexes, generated hash columns and stored-hash check constraints.
These controls explicitly select storage version v1.5.0, which C++ requires for
VARIANT columns. The initial default-v1.0 rejection is preserved before any
VARIANT table was created (`cpp-storage-controls-1`); the corrected reference
controls all pass (`cpp-storage-controls-2/verification.json`). Cross-engine
journeys require a new signed shipping library after the SDK gate passes.

## Signed SDK source-4 acceptance checkpoint

The immutable signed release SDK execution now passes all 55 exact C++ controls:
standalone and multi-argument hashes for constants, three stored rows and two
filtered selections (660 expected hash comparisons). This includes the seven
additional empty/nested JSON shapes. The prior empty-object failures remain
preserved independently. All 905 source inputs and the signed SDK identity
verify unchanged before/after execution. Evidence: external
`nested-hash-parity-20261027/{sdk-verification-3.json,sdk-run-3.log}`.

The supported nested-combination and empty JSON-container fixes now pass this
component gate. Shipping CLI/C API/SQL runner images are rebuilding from source4
for independent public-API and eight bidirectional plain/encrypted persistence/WAL
journeys. Final full SQL/shell/selected clients/ADBC and coupled performance
acceptance remain open; no whole-port completion is claimed.

## Signed shipping/public and persistence checkpoint

The native release build exits successfully; immutable CLI, SQL runner and C API
library copies are signed and strictly verified. The shipping library retains
547 C API exports and zero ADBC test hooks; SHA-256 is
`889c74d3bb17a0615a6898eea1a8457cbdf0b9dee47bf759290865b14e93a1ad`.
All 55 public hash/group/join controls match C++ literally. A fully initialized
5,000-row mixed VARIANT/nested-struct workload matches all hashes, 3,623 grouped
rows, selected hashes and join results. All 905 source inputs/signatures remain
stable. Evidence: `native-images-1.json`, `public-hashes-2/verification.json`.

The first mixed workload includes NULL primitive padding: its raw hashes differ
from C++. Independent native physical probes establish that C++ repeats differ
from each other (423/5,000 hashes), entirely due to unused BLOB memory; logical
bytes/metadata match and all normalized hashes agree. Rust retains initialized
bytes. The exception is explicitly limited in `conformance/divergences.md`.
The first raw failure stays preserved in `public-hashes-1`; accepted stage2
separately checks fully initialized raw hashes plus explicit padded-value
normalized hashes and literal logical grouping/join results. No broad hash
normalization or expected-value masking is applied. The initial physical probe
failed in the probe reader on invalid NULL-parent list offsets; the corrected
reader respects validity and its results are retained in `physical-2`.

All eight native cross-engine storage journeys pass: both directions, plaintext/
encrypted, checkpoint/nonempty crash-preserved WAL, hash-expression indexes,
generated hash columns, stored-hash check constraints, receiver insertion/update
and original-writer readback. Pre-replay file/WAL snapshots and every child image
identity are preserved in `storage-1/verification.json`. Each uses v1.5.0 storage
required by C++ for VARIANT and the explicit reference crypto-write fallback
setting with synthetic keys. These are real file journeys, not serialization-only
checks. Broader corpus/C API/ADBC/selected clients/shell/performance gates remain.

The new signed shipping C API suite passes all 30 files/155 cases/727,530 assertions.
Each executable's loader log selects the immutable library; its source/harness
hashes and strict signatures verify. Evidence: `capi-suite-1/verification.json`.
The affected 760-file SQL cohort is active, selected by VARIANT/Parquet/aggregate/
join/ART/JSON paths from the frozen corpus. It uses one worker with normal engine
threading, preserves complete fixture hashes and compares every selected verdict
against the prior accepted full run plus literal C++ skip reasons. Its final
verdict remains pending; this does not replace final full 4,870-file verification.

## Affected SQL acceptance checkpoint

The 760-file affected cohort finishes and passes its scoped regression gate:
752 pass, seven exact C++ skips and one existing documented failure
(`read_json_memory_usage`: query succeeds within the limit where C++ expects OOM).
No file verdict changes, crashes, timeouts or skip-reason differences occur.
All 905 inputs, signed runner and complete 4,870 SQL/1,522 data fixtures verify stable
before/after. Evidence: `affected-sql-1/verification.json` and fixture reports.
This is affected coverage, not final full-corpus/client/shell/performance acceptance.
The next core audit measures the live work queue's batched COPY memory-manager gap
before selecting its implementation; existing memory SQL tests alone do not
measure actual peak RSS or prove the internal manager is present.
