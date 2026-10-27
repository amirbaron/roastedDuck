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
