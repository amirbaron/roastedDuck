# Checked arithmetic and WAL commit accounting

Current-source development corpus953 exposed nine real arithmetic failures that
were absent from the older optimized260 results. The original verdicts remain in
952/967 and recovered inventory968. This checkpoint fixes those failures without
changing SQL expectations, the ratchet or the lower-memory cache policy.

## Changes

- Hash-table build-size estimation explicitly wraps its unsigned multiplication
  before converting to floating point, matching C++ `GetBuildSize` and existing
  optimized behavior. Saturation or float-first multiplication would change it.
- Join-filter dense-range detection uses checked signed subtraction. A gap wider
  than HUGEINT is not dense, matching C++ `TrySubtractOperator`; it must not panic.
- List slicing explicitly wraps index advancement for extreme steps, matching
  C++'s unsigned `idx_t` arithmetic, including advancement after the last selection.
- WAL append initialization resolves physical EOF before capturing the rollback/
  commit-byte baseline. `NO_WAL_WRITES` can accumulate an estimated size without a
  file; checkpoint-marker writes can initialize a smaller physical file. Capturing
  the estimate first caused subtraction underflow. The fix preserves estimated
  no-write accounting and uses the physical baseline for actual append accounting.

## Fresh signed verification

Evidence root: `.context/artifacts/platform-matrix-20261029/` on the external SSD.
Strict workspace/all-target Clippy974 passes. Actual Cargo exit0 and final
`build-finished:success=true` receipts975/978 precede execution. Shared976, SDK977,
SQL tools979 and integration binaries982 are frozen/content-authenticated with
caller-pinned CMS and normal strict Mac codesigning. Shared976 SHA-256:
`0e43cddd7a46919e52eb828a575cfe1babd97c543a7281fe5ac2b9027127ab50`.
Native exports remain547 duckdb-prefixed functions without private ADBC test hooks.

| Gate | Result | Evidence |
| --- | --- | --- |
| Exact original failures | All9 pass, unchanged inputs | cohort980/originals |
| Affected optimizer join,attach,checkpoint,list files | 190 pass/5 skips; all195 verdicts and exact skip reasons match native C++ reference952 | cohort980/affected |
| Persistence integration | All11 pass | signed982, runtime984 |
| Registered-storage integration | All9 pass | signed982, runtime984 |
| Full public C API | 30 files/155 cases/723124 assertions pass | CAPI983 |

The nine original files cover two join-planner overflow cases, four
NO_WAL_WRITES recovery/checkpoint cases, HUGEINT join limits, list extraction
across types and extreme signed list-slice steps. Persistence checks include
checkpoint/reopen, nonempty-WAL replay, torn tails, indexes, prepared checkpoint
rollback/reuse and bulk storage. Registered-storage checks include replay,
truncation before receiver writes, backend retention and typed open errors.

Runtime982 initially inherited the host+02 timezone and failed a UTC text
expectation, while preserving the same instant. Its log remains. Runtime984 runs
the same signed executables with explicit `TZ=UTC`; no expected value or engine
code was changed for that harness setup. Assertion-loop totals are not a progress
or performance metric.

This is a Mac ARM64 checked/development checkpoint. Optimized/release boundary
verification, wider WAL fault-injection journeys, all final platform/client gates
and full common-source release verification remain open. The195-file cohort is
not a replacement for the full4870 corpus on eventual final artifacts.

## Original corpus and reference exceptions

Complete C++952:4795 pass/73skip/1metadata fail/1resource crash. Rust968:4771 pass/
73skip/17fail/9crash, explicitly3865 preserved completed-prefix verdicts plus1005
same-image/profile continuation967 after verified stopped execution966. Exact lost
prefix crash exit codes are not fabricated. No completed file was retried to get a
better result. The nine arithmetic failures are fixed in this targeted checkpoint;
original recovered counts are never rewritten to infer an unexecuted full pass.

Native C++ unittest969 independently reproduces the unset optional catalog-entry
pointer in `duckdb_dependencies()`:1case,215/216 assertions pass, then database
invalidation. Rust passes the original case. Reference classification is distinct
from accepting a Rust failure.

Six fixtures requiring8–16GB RAM pass on BOTH engines in970 with identical16GiB
engine defaults/20GiB sampled-RSS guard. Both original7GiB Zstd guard kills remain.
Other resource fixtures also pass on both engines in971, under the same16GiB/20GiB
profile and900-second diagnostic bound; original300-second results remain intact.

| Individual resource diagnostic | C++ ms | Rust ms | C++ sampled RSS KiB | Rust sampled RSS KiB |
| --- | ---: | ---: | ---: | ---: |
| 4.3008GB gzip round trip | 159736 | 95280 | 4458128 | 8660144 |
| TPC-H options | 23376 | 27342 | 5988896 | 6275136 |
| Giant-string Zstd | 3994 | 6524 | 9816864 | 9832464 |

Samples are0.5-second observations, not exact peak RSS, and this is not macro
performance acceptance. The gzip memory outlier remains serious despite its
faster time. Reclaim-space batch insert also retains a separate whole-file timing
outlier124565ms versus C++21864ms. Those timings compare checked/development Rust
with release C++ and include process/setup work; final release measurements are
required before accepting or attributing the gap. No caching is added to close it.

## Remaining acceptance

The goal remains active and incomplete. Preserve21 tracked buffer/cache/batch-
memory/Parquet COPY inputs and the accepted roughly15–20% longer execution with
60–70% less memory on TPC-H/ClickBench. The accepted COPY throttling exception stays
conditional on the final macrobaseline; individual query outliers remain explicit.

The owner resolved the dependency boundary on2026-11-01: third-party native crypto
is allowed while the DuckDB engine remains Rust. Still required: broader
functional/HTTPFS/lifecycle cases, final
signed common-source SQL/shell/C API/ADBC and Python/Go/Swift/platform matrices,
cross-engine checkpoint/nonempty/torn/encrypted database-WAL journeys and coupled
native speed/RSS acceptance. Required targets remain Mac ARM64, iOS ARM64 device/
simulator and Linux ARM64/x86_64; Mac Intel and extra clients remain outside scope.
