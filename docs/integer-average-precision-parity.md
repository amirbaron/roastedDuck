# Native integer-average precision — 2026-10-30

The Linux corpus exposed two real precision failures: wide integer `avg` and
`list_avg` rounded the128-bit sum to DOUBLE before division. For the HUGEINT values
9,007,199,254,740,992,1,0, Rust returned3,002,399,751,580,330.5 where Linux C++ returns
3,002,399,751,580,331. The preserved Mac arm64 control skipped these tests through
their unchanged `require longdouble` condition.

The integrated candidate now follows C++'s native intermediate rounding. Signed
Linux debug and optimized release tests each pass **46 functions-library tests and
four SDK regressions**; workspace/all-target Clippy passes with warnings denied.
The fresh signed native release artifacts pass the514-file affected numeric SQL
gate:510 pass,two exact matching skips and two preserved exceptions,with only the
two intended failure-to-pass changes. This does not establish final release or
whole-port acceptance.

## Target behavior and implementation

| Target | C++ intermediate precision | Candidate finalization |
|---|---|---|
| Linux arm64 | binary128,113 significant bits | Fixed-size unsigned integer rounding/division,then DOUBLE |
| Linux x86_64 and Apple Intel targets | extended80,64 significant bits | Same arithmetic at64 significant bits,then DOUBLE |
| Existing binary64 targets,including macOS/iOS arm64 |53 significant bits | Preserve the existing two-half HUGEINT conversion and DOUBLE division |

The sum/count aggregate state and row update/combine paths are unchanged. No new
dependency,table cache or retained data is introduced. The correction is confined
to wide integer and decimal finalization; narrow integer,DOUBLE and temporal
averages retain their existing paths. Power-of-two divisors use an exact exponent
adjustment. Other divisors use bounded significand arithmetic and round to nearest,
ties to even at the native intermediate precision and again at DOUBLE precision.

These separate rounding steps matter: directly rounding an exact rational result
can disagree with native C++ on double-rounding boundaries. Boundary controls
include2^113+2^60+1 and the distinct extended80/binary128 results for2^64+2^11+1.

## Evidence

Evidence root: `.context/artifacts/platform-matrix-20261029/`, on the dedicated SSD.

- `work/extended-average-matrix-1/`:219,668 inputs covering signed128-bit limits,
  count boundaries through UINT64_MAX,decimal scales,negative values and rounding
  ties,including20,000 deterministic random inputs.
- `extended-average-freeze-1.json` and
  `work/linux-arm64/extended-average-signed-1/`: signed standalone prototype and
  C++ oracle. The oracle links the actual frozen C++ HUGEINT conversion.
- `work/linux-arm64/extended-average-matrix-run-1/verification.json`:
  **219,668/219,668 literal DOUBLE bit patterns match**,zero differences; actual
  native `LDBL_MANT_DIG=113`. Signatures and selected reference library checked
  before/after execution.
- `extended-average-intel-1/verification.json`: **219,668/219,668 bit patterns
  match**,zero differences for native extended80 expressions under Rosetta.
  The C++ expression is copied from the frozen conversion and uses the real Intel
  ABI; this is not a frozen Intel engine test or physical Intel performance result.
  Both Mac executables are signed before execution.
- `source-9/manifest.json`: integrated candidate plus regression tests and a
  portable conversion in the existing row benchmark timer;974 guarded inputs.
- `average-sdk-build-4/` and `work/linux-arm64/average-sdk-build-4/`: successful
  workspace/all-target Clippy and completed test builds,source hashes unchanged.
- `average-sdk-freeze-1.json` and `work/linux-arm64/average-sdk-run-1/`:
  signed integrated debug executables;46+4 tests pass with no failures/ignored
  cases and swap disabled. Scalar,mean,list,grouped/window,decimal,NULL and zero
  behavior is checked. Native signatures verify before/after.
- `average-release-freeze-1.json` and `work/linux-arm64/average-release-tests-1/`:
  optimized shared/static libraries,CLI,SQL runner and two regression executables
  are frozen and signed; the same46+4 tests pass without failures/ignored cases,
  with swap disabled and signed integrity checked before/after.
- `average-numeric-preparation-1.json` and `average-numeric-sql-1/`:514 unchanged
  numeric/list/window/decimal/generator/VARIANT files complete against the signed
  release runner:510 pass,two matching skips,one existing C++-extension exception
  and one lower-memory exception. Both original precision regressions pass; no other verdict or skip
  reason changes. Every4,870 SQL/1,522 data hash/link and all three signed artifact
  groups remain unchanged before/after the cohort.

The optimized library SHA256 is
`6066d376cd6c75794bb16a0be61a791b1b6d3688752c636213e61414445e1327`;
CLI `1cec4a3e24793a4f4428ee996f7a05a2f2ed2215009a5382822d03277f08382b`;
SQL runner `2a93e0a1c5f57c197ae99ae4b73e7e24237697ef79e5b955c016bb42c466f284`.

The corrected Linux C API harness passes all30 files/155 cases on each engine:
C++710,396 assertions; Rust783,150 assertions. Assertions involving bounded pending
loops vary with runtime; test cases and upstream bodies are unchanged. Eleven
literal public-C-API average controls match DOUBLE bits and NULLs across the frozen
C++ and current Rust library,including scalar,mean,list,window,decimal and rounding
boundaries. The Rust library retains547 DuckDB exports and zero ADBC test hooks.
Evidence: `work/linux-arm64/average-capi-{cpp,rust}-2/verification.json`,
`average-capi-literals-1/verification.json` and `average-native-exports-1.json`.

The first C++ control crashed because the test harness exported its incompatible
stand-in `duckdb::Value::TIME_NS`. ELF binding diagnostics show the reference
library calling that executable helper; a separate public C API probe passes on
both engines. Hiding test-only C++ definitions fixes all155 reference cases without
changing engine code or upstream assertions. The original failure,successful-assert
trace,symbol audit and binding lines remain under `average-capi-cpp-1/`,
`average-capi-cpp-types-diagnostic-1/`, `capi-symbol-audit-1.json` and
`capi-helper-binding-1/`. This was a harness fault,not a reference-engine exception.
The same visibility protection is prepared for the Linux ADBC harness,whose runtime
verification remains pending. Harness-only snapshots10/11 leave engine inputs and
the signed6066 library unchanged.

The earlier build attempts are retained: documentation/literal formatting lints,
then an existing Linux benchmark timer conversion error (`tv_usec` is64-bit on
Linux and32-bit on Darwin). The portable conversion fixes the example build;
the AVG arithmetic is unchanged by these follow-up edits. Upstream SQL inputs,
reference results and ratchets are unchanged.

## Remaining verification

Complete the final integrated core/client/storage/WAL/platform gates. Mac binary64 behavior and Intel
engine integration still require their target checks.

The earlier full Linux corpus is a pre-fix diagnostic: Rust4,777 pass/75 skip/
17 fail/one shared memory-cap crash;15 failures are existing documented exceptions,
two are this precision bug. C++ has4,793 pass/75 skip/one memory-cap crash/one timeout.
The shared4.3GB gzip resource failure and C++ VARIANT TPC-H timeout remain queued
for separately labeled correctness follow-ups.

The accepted lower-memory performance policy is unchanged. No performance acceptance
is inferred from arithmetic bit comparisons or debug tests. Final coupled speed/RSS
measurements and individual outlier dispositions remain required; whole-file timing
observations from the diagnostic corpus are not query benchmark results.
