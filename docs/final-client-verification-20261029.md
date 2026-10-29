# Current signed client verification — 2026-10-29

The goal remains active. This checkpoint verifies the selected clients and existing
ADBC on the current Resident source1 candidate. It does not establish untested
platforms, close the remaining core contract audits, or grant final performance
acceptance. No table cache or increased memory limit was introduced.

## Candidate and evidence

The native source manifest is `art-resident-checkpoint-20261029/source-1/manifest.json`
(967 guarded build inputs). The immutable signed native library SHA256 is
`01aaeb24d068580f7755656062ba7a3e8a31bbbdfd4a1c5933379a59ecb0a702`.
The new signed Python wheel SHA256 is
`a0724fd39d60f063ad62968d75b0e4b52faeb0de6ec19c57d6c32b8d28399375`;
its native extension SHA256 is
`3b9c8b632ee1055cd7f671b36ac765c5718a929350979fe41ff7aca6666f0ff9`.
Both use the same guarded source inputs. Native signatures, selected engine images,
and source hashes are checked before and after their runs.

New evidence lives under `.context/artifacts/final-verification-20261029/`, whose
external root is `/Volumes/amirs ssd/ai context/conductor/duckdb-rust/ottawa-50198a764d3b1fdc/`.
Tests/builds run sequentially at reduced priority. Native harnesses, Go executables,
Swift bundles/frameworks and Python extensions are signed before execution.

## Verified selected-client gates

| Gate | Current candidate result | Evidence relative to task |
|---|---|---|
| Python Arrow19 full fast | 3,205 pass, 27 skip, eight xfail, one existing shared xpass; all 3,241 JUnit records exactly match C++ | `fast-both-verification.json`, `full-fast-arrow19-1/` |
| Python Arrow21 full fast | 3,455 pass, 28 skip, eight xfail, one existing shared xpass; all 3,492 records exactly match C++ | `full-fast-arrow21-1/` |
| Frozen upstream inputs | All 300 upstream test/data files match the pinned release archive | `run-fast-both.py`, `fast-both-verification.json` |
| Additional Arrow producer controls | 62 literal C++ controls per Arrow19/21 environment pass | `python-arrow-1/verification.json` |
| Python non-fast | All seven selected files: 71 pass/eight exact shared skips on each engine | `nonfast-1/verification.json` |
| Installed Python ADBC | Both environments pass Python/UDF-to-ADBC transaction/rollback/commit/reopen handoff; exactly one native engine loaded | `python-supplement-2/verification.json` |
| Python stubs/typing | Six pinned native stubs plus forwarding stub exact in both environments; two strict positive clients pass; each negative client produces its four expected errors | `python-supplement-2/` |
| Python availability | All 385 frozen runtime exports present in each environment; 29 typing/protocol declarations remain separately classified | `python-contract-2/verification.json` |
| Python encrypted Parquet | 24 local/registered-memory cross-engine cases per Arrow environment, including three key sizes, prepared writes, COPY FROM, nested rows, plan serialization and exact corruption/error messages | `python-encryption-arrow{19,21}-2/verification.json` |
| Python persistence | Eight bidirectional 15-column checkpoint/nonempty-WAL, append/mutation/readback journeys pass across both environments | `python-persistence-1/verification.json` |
| Native ADBC | All 64 frozen cases/3,040 assertions pass on both engines, without filters | `native-adbc-1/verification.json` |
| Go | Complete pinned root suite including Arrow: 652 passes, exact frozen C++ case IDs/verdicts, no unfinished/unstarted tests | `go-verification-1.json`, `go-test-1/summary.json` |
| Separate Go module | Prepared typed insert, Appender, NULL/decimal/temporal/BLOB/nested values, duplicate-key error and rollback; four bidirectional checkpoint/nonempty-WAL mutation/readback journeys pass | `go-consumer-1/verification.json` |
| Swift | Fresh C++ and Rust each pass 132 pinned upstream cases plus one separate identity test, exact case IDs | `swift-verification-1.json`, `swift-1/{cpp,rust}-test/summary.json` |
| Swift distribution | Relative XCFramework, relocated SwiftPM consumer, standalone consumer/framework and four bidirectional checkpoint/nonempty-WAL journeys pass | `swift-1/{sdk,consumer,standalone,interop}/` |

The fast-suite xpass is the existing shared TIME-to-TIME_NS insertion outcome. JUnit
includes it among 3,206/3,456 successful records; it is not an additional ordinary pass.
The non-fast skips include Windows-only crash coverage and unset configured HTTPFS
requirements. The H2O transport plugin replays five pinned official responses;
upstream SQL/assertions remain unchanged. This does not prove configured HTTPFS.

The native ADBC test bridge has three opt-in helper exports, in a separate signed
library. The normal feature-free build was restored before running either harness;
the immutable shipping library retains 547 C API exports and zero test hooks.
The frozen native C++ reference source is 1.5.6; its local build literally reports
`v0.0.1`. Reports preserve that metadata. The independent official Python reference
reports release 1.5.6. No C++ engine is bundled into the Rust client candidates.

## Individual Python performance outliers

Three paired/interleaved fresh-process samples per storage kind, one million rows,
one engine thread, current release wheel, identical count/length checks. Values are
separate medians; RSS is whole-process decimal MB, including input and interpreter.

| String storage | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB |
|---|---:|---:|---:|---:|---:|
| Pandas Python strings | 35.09 | 60.71 | 1.73 | 142.11 | 139.46 |
| Pandas Arrow strings | 58.82 | 112.97 | 1.92 | 198.02 | 212.43 |

All twelve checks pass. These are remaining client performance outliers, separate
from the accepted macro speed–memory baseline and COPY tradeoff; no acceptance is
inferred from an aggregate. Raw helper text still says “Rust dev”; the current-run
receipt explicitly corrects that historical label and pins the release wheel.
Evidence: `python-contract-2/verification.json` and every raw sample JSON/log.

## Remaining acceptance

Full signed TPC-H SF10/ClickBench at one/14 threads is complete under
`performance-1/`: all130 configurations pass, with warmup plus three timed repetitions,
checked answers, wall/CPU/whole-process and interpreter-adjusted RSS, actual engine
images, frozen queries/dataset hashes and paired outlier follow-ups. The current
native candidate meets the existing aggregate policy; weighted sums, peak RSS and
all63 query outliers are explicit in [native performance acceptance](performance-final-native-20261029.md).
The Python-only object-buffer candidate is building separately and has no acceptance yet.

Current runtime/package evidence is macOS arm64, Python3.13 with Arrow19/21,
pinned Go v2.10506.0 and Swift6.4. The earlier release-platform question is pending;
no untested OS/architecture is implied. Remaining broader contract/configured backend
and memory-accounting audits in `work-queue.md` need explicit evidence/disposition.
Existing documented C++ reference defects remain in `conformance/divergences.md`.

## Retained harness failures

The first Python supplemental driver completed all eight persistence journeys but
failed its final receipt while hashing an obsolete helper path. The finish verifier
checked all 24 completed child terminal records, selected-image identities, result
hashes, and pre-replay snapshots without repeating or discarding the successful
journeys. Original failure log remains `python-supplement.log`.

The first runtime-export verifier incorrectly required all 414 stub declarations to
be runtime exports. Its preserved output correctly showed 385 available/29 typing
only; the corrected run compares the exact available set against the frozen control.
No runtime APIs, tests or expectations were changed to make this check pass.

## Bounded memory-accounting observations

Eight fresh C API processes compare the existing200k-row random CTAS at one/14
threads under1/4MiB. All queries return the correct200k count. At1MiB, Rust retains
1,067,008 bytes of IN_MEMORY_TABLE buffers versus804,864 for C++; at4MiB both retain
2,377,728 bytes. This confirms the remaining one-block residency difference from
`work-queue.md`, without proving its cause or closing the accounting audit. The
pipeline chunk path uses heap RawBuffer allocations rather than C++ BufferAllocator;
source inspection and the bounded observation are distinct evidence. No synthetic
counter adjustment or higher memory budget was introduced.
Evidence: `vector-accounting-1/observations.json` and all eight raw reports.
