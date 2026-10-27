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
