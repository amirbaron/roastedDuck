# Parquet schema mapping checkpoint

2026-10-29/30; the full goal remains active. Inferred schema evolution now maps
requested struct fields by name and uses the existing `remap_struct` builtin for
non-trivial STRUCT/LIST/MAP changes. Missing nested fields become typed NULLs;
explicit `schema` definitions retain C++'s whole-column cast/read behavior.
Cast-binding errors retain their original category, while value-conversion failures
keep the extended multi-file context. Database bindings use the real connection
function client, and the public standalone `bind_files` API remains compatible.

## Confirmed gaps and controls

The preserved native Rust library read a damaged unselected sibling whenever a
struct's file type differed from its global type. Frozen C++ returned the selected
field successfully. The original files/footer hashes, damage offset and before
outputs are retained. Reordered and nested fields reproduce the same issue.

Another control uses inferred `STRUCT(a INTEGER,b STRUCT(x INTEGER))` followed by
`STRUCT(a BIGINT,b STRUCT(y INTEGER))`. C++ returns a=1..4 and b.x=10,20,NULL,NULL;
the preserved Rust whole-STRUCT cast rejected the disjoint nested members. Whole
values are compared through explicit VARCHAR casts; deprecated raw C API complex
value getters returning NULL are not used as evidence of logical NULL results.

Explicit-schema controls prove a different contract: even an unselected incompatible
member must still cause the reference conversion error, and identical explicit
schemas still read/reject a damaged sibling. Disjoint explicit structs prepare
successfully on both engines, then execute with a raw Binder error. Rust previously
wrapped that error as Conversion. All these boundaries have SDK regressions.

## Correctness verification

The source7 functional candidate passes workspace/all-target Clippy, nine signed SDK
tests in both debug/release, and106 affected library tests (two unchanged Snappy
comparison/benchmark tools ignored). Its signed C API library is
`a93a18202e633129a78182c02ee905ff2c062f3fb3424489be4b20df0ec6ace9`.
Fresh paired C API controls match11 cases literally, including prepared error timing
and complete error messages for explicit casts. Two malformed-Thrift negative
controls reject on both engines but expose a message difference; it is retained,
not counted as literal error parity.

Source8 replaces per-row remap value copies with references to vector fields and
string heaps. It retains the established value path for sparse container selections
rather than materializing storage for unselected parents. Identity nested subtrees
share their owners; STRUCT/LIST/MAP remaps preserve validity, offsets and defaults.
Two added unit tests cover string ownership after releasing dictionary inputs and
sparse list selection/defaults. Source8 passes workspace Clippy, nine signed SDK
tests and108 affected tests with the same two ignored tools.

Source9 additionally validates compact type tags before generated readers ignore
fields or empty collections, matching C++ `getTType` diagnostics for invalid tags.
It passes workspace Clippy, nine signed SDK tests in both debug and release, and
109 affected tests with the same two ignored tools. Fresh native images, literal
C API comparisons and coupled measurements are rebuilding/running in sequence;
no final source9 artifact acceptance is claimed before those checks finish.

## Rejected performance candidate

The source7 scalar remap implementation copied every row's nested values and strings.
Paired source7 measurements exposed a real regression, despite correct values:

| Whole-struct scan | C++ ms / MB | Preserved Rust ms / MB | Source7 ms / MB |
|---|---:|---:|---:|
| One thread |15.55 /59.87|14.60 /60.82|21.37 /83.48|
|14 threads |2.90 /118.49|2.86 /119.29|8.61 /162.23|

Projected scans used less memory but also slowed down (14 threads:1.59→7.04ms,
84.15→48.02MB). These measurements use the same65,536 rows,1,026-byte payloads,
three timed samples plus warmup, checked sums/counts, and disabled external file
cache. All whole-process RSS and samples are retained. Source7 is performance-
incomplete; no aggregate or policy acceptance conceals the regression. Source9's
vector implementation must pass fresh coupled measurements before acceptance.

## Evidence and remaining gates

External evidence: `.context/artifacts/schema-evolution-projection-20261029/` contains
source1..9 snapshots, original/corrected probes, input hashes, signed SDK runs,
affected-test receipts, native1 identities/matrix and `profile-1/verification.json`.
The external root is
`/Volumes/amirs ssd/ai context/conductor/duckdb-rust/ottawa-50198a764d3b1fdc/`.

Remaining: release source9 SDK/native images, literal malformed-message comparison,
repeat speed/RSS against both preserved and rejected candidates, expanded SQL and
complete final integration. Earlier130-query TPC-H/ClickBench acceptance is a
preserved artifact control, not acceptance of this later core change. Required
platforms now include macOS/iOS devices/simulators and Linux x86_64/arm64; the other
Apple families and Windows are outside scope. See the release platform matrix.
