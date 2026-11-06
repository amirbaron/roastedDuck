# Wide-schema query outlier investigation

The original canonical 20,000-column/10-row hash-join test is an open severe
outlier. A paired source-46 public C API diagnostic preserves all five original
statements and verifies all 200,000 HUGEINT cells and 20,000 names independently.
It separates prepare/execute times, 10 ms sampled phase RSS and cumulative kernel
process peak RSS. Execution is Linux x86_64/Rosetta, not native performance evidence.

| Phase | C++ prepare / execute | Rust prepare / execute |
|---|---|---|
| Wide probe CREATE TABLE AS | 0.363 / 0.492 s | 14.080 / 14.646 s |
| Wide SELECT/join | 0.373 / 0.511 s | 20.428 / 22.027 s |

Whole-process kernel peaks are 649,703,424 bytes for C++ and 2,227,556,352 for Rust.
This is worse on both measures; it is not covered by the accepted lower-memory
tradeoff. The diagnostic's prepare/execute calls can repeat work compared with
the canonical statement runner; retain each measurement's scope.

Static inspection finds linear case-insensitive map lookup in repeated binding
and schema construction. The current unverified candidate adds a lazy offset-only
lookup index for maps of at least 256 entries while retaining entry order, original
byte spelling, first duplicate semantics, and existing hash/debug/equality behavior.
Small maps retain linear storage. This indexes metadata; it does not change table
page caching or buffer budgets. New tests cover invalid UTF-8/NUL keys, duplicate
removal, collision resolution, cache-independent traits, and small-map allocation.
Build/test and identical paired time/RSS verification remain pending. The memory
excess is a separate open investigation; this candidate is not accepted yet.

External evidence: `work/linux-amd64/widejoin-phase-pair-72/verification.json`,
`widejoin-phase-inputs-71.json` and the retained failed attempt
`linux-amd64-widejoin-phases-71/` (input manifest mount error before native execution).
Final native macro/outlier acceptance remains required.

The first optimized map-index candidate (source75/release76) passes strict common
quality checks, all39 signed common unit tests and all155 original C API cases.
The identical diagnostic77 reduces probe preparation14.08→4.60s and join
preparation20.43→8.13s. Whole-process peak RSS is still2,332,221,440bytes versus
C++621,092,864bytes. The outlier remains unaccepted; the map improvement alone
does not solve time or memory parity.

The next unverified candidate indexes large case-insensitive sets too, preserving
small-set storage and visible traits. It also lets audited projection and
table-function scan operators start with typed empty output vectors because they
replace those vectors completely. Other operators retain preallocation. This
avoids allocating buffers that are immediately discarded, while retaining Rust's
initialized-buffer requirements. Repeated-batch tests check NULL/HUGEINT/string
values and result ownership after inputs are dropped. New build/runtime and
identical coupled verification are pending; no table cache/budget increase.

Source81 strict checks and all55 signed common/execution units pass. Release82
also passes155 C API cases. Diagnostic83 reduces process peak to1,675,358,208bytes
(C++613,277,696). Probe execution samples350MB versus C++510MB, but wide-join
execution still samples1.57GB versus590MB, with7.996s preparation/8.590s execution.
This remains an unaccepted outlier. The next candidate adds safe deferred buffers
for audited seq-scan/hash-join paths, replaces CTAS repeated column-name scans
with the existing set, and indexes normalized parsed column references while
retaining general expression equality. New suffix/equality tests are pending.

Source87 strict checks and all57 signed common/planner/execution units pass.
Optimized release88 preserves all547 exports and passes all155 original C API
cases. Diagnostic89 reduces probe preparation to0.294s (C++0.378) and join
preparation to0.355s (C++0.436); join execution remains0.823s versus0.426.
Peak process RSS is1,018,195,968bytes versus612,667,392, down from2.33GB but still
1.66times C++. All200,000 cells and20,000 names remain correct. The severe binding
cost is repaired in this component, but wide-join memory/execution and final native
macro/outlier acceptance remain open. These are single paired translated diagnostics,
not an accepted final performance distribution. Evidence: release-pipeline88 and
work/linux-amd64/widejoin-phase-pair-89/verification.json.
