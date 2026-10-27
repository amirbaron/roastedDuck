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
