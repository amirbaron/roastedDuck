# Current verification boundaries and remaining exceptions

Observed November 6, 2026. The signed source2551 software candidate has completed its recorded software verification, performance review and final relocation checks. The [delivery index](release-candidate-index-20261106.md) links the archive and exact evidence. The goal is complete under the owner-approved scope. Physical iOS execution/provisioning remains **unverified**, with the owner’s explicit physical-device test exception recorded in the signed delivery addendum.

| Observation | Recorded disposition |
|---|---|
| Fourteen raw Rust SQL failures per platform | Eleven use private C++ engine-internal extension fixtures. Three queries succeed where the test expects OOM under the original settings. Raw failures remain; success does not prove peak RSS or execution of branches after the failed expected-error assertion. Separate public API and memory checks retain their own scope. |
| Mac C++ metadata-concurrency failure | The original reference failure remains in the full collection. It is neither a Rust failure nor a reference pass. |
| Linux x86_64 float/decimal reference SIGSEGV | Original worker failure/core retained. Rust passes the unchanged case on current targets; 30 C++ and 30 Rust replays pass. Cause remains unknown and unapproved; no C++/Rosetta cause or crashed-case speedup is inferred. |
| Linux x86_64 shell reference SIGSEGV | Original `byte-b0/missing-init` crash and 544,509,952-byte core retained. Five unchanged Linux paired replays and one Mac control match; root cause is not established. [Incident2789](../.context/artifacts/platform-matrix-20261029/reference-shell-incident-2789/verification.json) remains unresolved and unapproved. |
| Shell metadata/native frames/invalid UTF-8 | Compiler/source IDs and native frames remain explicit differences. Some invalid UTF-8 inputs trigger reference render errors while Rust agrees with the same-input Mac reference. Exact universal byte parity is false. All original inputs, outputs, exits and strict case-specific reviews remain. |
| Mac filename fixtures | The original 399 Mac filesystem fixture errors remain raw. The same original names/bytes pass on both Linux targets, with exact transport and selected-native checks. This is filename coverage, not opaque database-file byte equality. |
| Shared shell skips | Mac has 393 upstream passes/two skips; Linux has 392/three. The original HTTP logging case now actually passes on both engines and both Linux architectures with an authenticated offline prerequisite. The original full-suite skips were not rewritten. Other skips concern upstream file logging and unsupported `db_config`. |
| SQL prerequisites | Thirty original Mac skips and 28 per Linux target remain with matching reasons. Shared skips are not converted to functionality passes. |
| Python matching nonpasses | All five Mac and ten Linux normal ABI cells are qualified against C++ with raw matching skips/xfails retained. Linux records 61,486 PASS/784 matching nonpasses per engine; Mac records 30,743/392. [Full Linux index](../.context/artifacts/platform-matrix-20261029/current-Linux-Python-all-ten-index-2892/verification.json). |
| Workspace ignores | All 149 signed harnesses ran: 1,089 passes, no failures, seven unchanged ignores. Two required ignored functional controls ran separately and passed; five diagnostic ignores remain unpassed. |
| Upstream Swift warnings | The unchanged upstream Swift package emits Sendable/pointer warnings in its declared language mode. Builds and tests pass; strict Swift 6 language-mode conformance is not claimed as a new Rust-port feature. |
| Go linkage | The verified contract uses `duckdb_use_lib,duckdb_arrow` and the selected Rust native package. The upstream default prebuilt C++ path is outside that contract and is not included in the shipped payload. |
| Physical iOS | Fresh discovery found 12 simulated devices and no physical iPhone/iPad. Simulator tests and signed device builds do not prove hardware execution/provisioning. This remains unverified; the owner explicitly accepted a physical-device test exception on November 6, 2026. |

## Performance acceptance has a defined scope

The [current native report](performance-final-native-20261106.md) accepts the existing aggregate geometric-mean speed/memory policy and the macro condition attached to the approved 14-thread/256MB COPY tradeoff. No engine cache or larger query-memory budget was introduced.

The report does not claim independent per-query parity. ClickBench summed best times are 22.9%/25.4% longer, with medians 21.6%/22.7% longer; they do not meet a strict 20% cap. One-thread maximum RSS is 13.270GB C++/13.116GB Rust, almost equal despite the geometric-mean saving. Every one of the 63 flags, all 130 query rows and every timed sample remain visible.

The five original whole-file stress cases remain explicit limitations: batch reclaim 3.115x time/0.774x RSS, VARIANT 5.391x/0.376x, compressed CSV 1.494x/1.102x, checkpoint/index 3.042x/0.593x and primary-key reclaim 2.509x/0.977x. All 30 runs pass. These are not TPC-H/ClickBench measurements and are not averaged into their acceptance figures.

Single-thread COPY is 37.5% slower with 46.0% more median whole RSS. The 14-thread/512MB profile is 33.8% slower with 18.6% less median RSS. The accepted 14-thread/256MB profile is 15.6% slower with approximately equal median RSS. The skewed C++ attempts fail three times while Rust completes; their failure duration is not a successful speed comparison. All four profiles and 24 raw samples remain in the report. Separate [Python client rows](current-python-client-outliers-20261105.md) retain all best/median time and RSS observations without averaging away slower Python-backed scans.

## Orchestration failures retained during final verification

The first timing attempt read an obsolete phase-reference path and failed before collecting a sample. Recovery changed only the predecessor path, retaining all assertions and original timing/validation workers. Final signed Go path testing exposed `/usr/bin/nice` stripping `DYLD_LIBRARY_PATH`; a diagnostic reproduced the stripping without running DuckDB. Direct execution at the original low priority preserved strict path checks and passed. Swift's first final-path harness omitted a fixture directory and failed while writing identity information before SQL; directory setup was corrected without changing consumer source or assertions. All failed receipts remain on the SSD.

These repairs changed no engine bytes, source snapshot, query input, expected result, memory policy or raw outcome. The original signed payload and its independently copied/extracted versions retain all 300 file hashes and symlink targets. The candidate has not been publicly published. Historical source2253 results remain historical and are not substitutes for current evidence.
