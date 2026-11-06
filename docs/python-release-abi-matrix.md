# Python release ABI coverage

A live read-only audit of [DuckDB 1.5.6 release metadata](https://pypi.org/pypi/duckdb/1.5.6/json)
on 2026-10-30 finds CPython 3.10, 3.11, 3.12, 3.13 and 3.14 wheels for each required
desktop/server target: macOS arm64, Linux arm64 and Linux x86_64. The project's
`requires-python` floor is 3.10. No additional ABI is inferred from future Python
versions or from the floor alone.

| Required target | CP310 | CP311 | CP312 | CP313 | CP314 |
|---|---|---|---|---|---|
| macOS arm64 | open | open | open | signed component/client gates pass | open |
| Linux arm64 | open | open | open | signed component/client gates pass | open |
| Linux x86_64 | open | open | open | signed component/client gates pass | open |

This represents 15 ABI/target cells. A CP313 pass does not verify another ABI.
The completed Intel Mac CP312 checkpoint is retained but excluded from required
coverage because the owner removed Mac Intel support. iOS Python is not introduced
beyond the upstream client surface; iOS uses the embedded C API and Swift client.

Existing CP313 evidence is recorded in `macos-python-client-parity.md` and
`python-named-memory-parity.md`. Those are component checkpoints using their stated
artifact identities, not proof of a final common-source release. Final wheel
verification must pin the interpreter, source, dependencies, original C++ control
wheel and signed Rust native bytes, then execute applicable functional/client,
typing, ADBC, persistence/WAL and coupled diagnostic checks. Unsupported dependency
combinations must be recorded explicitly rather than forcing an older Arrow ABI
onto an interpreter for which upstream does not provide it.

The audit receipt `python-required-abi-audit-1.json` under external
`.context/artifacts/platform-matrix-20261029/` retains all matching wheel names,
Python/ABI tags and published SHA256 values for the three targets. This documents
required verification coverage; it does not waive an open cell or accept performance.

All 15 original C++ control wheels are now downloaded without alteration,
verified against published SHA256 values and inspected for the required native
architecture, then covered by signed content manifests. The combined receipt is
`python-required-cpp-controls-index-1.json`, with AMD64 inputs in
`work/python-amd64-control-inputs-1/` and the ten Mac/Linux ARM inputs in
`work/python-arm64-control-inputs-1/`. No native client code was executed during
this preparation, and none of the open Rust ABI cells changes status.

The Linux x86_64 CP313 component now completes both fast matrices and all recorded
package/typing/ADBC/object-boundary/non-fast/producer/named-memory/ArrowString/
persistence/encryption gates, with coupled client timing/RSS. See
[the x86_64 report](linux-x86_64-python-client-parity.md). Three of 15 cells have
component evidence; final common-source wheel acceptance remains separate.

## Current source226 verification (2026-10-31)

The signed Linux x86_64 CP313 wheel235 now has current-source evidence:
four complete upstream fast-suite comparisons (baseline/Pandas3 x Arrow19/21),
original and expanded Pandas3 string controls including an actual environment
without Arrow, and all nine package/typing/installed-ADBC/object/interchange/
persistence/encryption/lifetime/measurement supplements. Every case ID, verdict
and skip reason matches C++; the Pandas3-specific string test actually runs.
External aggregates250/253 retain exact identities and individual results.
This execution is under Rosetta, not native x86_64 performance acceptance.

The Mac/Linux arm64 CP313 receipts in the table above are historical components;
they still need the source226 refresh. Thus one of the15 required cells has this
new source's complete recorded component gates, two have older component gates,
and12 still have no completed required ABI cell. No ABI is accepted from tags,
interpreter presence, dependency metadata or a wheel build alone.

Linux x86_64 CP310 continuation263–266 is prepared behind the active full SQL
corpus260: dedicated clean build target, strictClippy/CargoJSON/Maturin/full source
checks, signed normal cp310 wheel, offline32-pin C++/Rust Arrow19/21 installs and
pip-check, native hashes before first import, both complete upstream fast suites
and all nine supplemental components with actual3.10 typing. These stages have
not run and were deferred321/343 for the confirmed C-ABI extension-loading core
gap341. Rebase them on the corrected common source; CP310 remains open. The signed compatibility helper166 and typing
fixtures171 preserve their original inputs. Later311/312/314 and ARM cells remain
open; unsupported dependency combinations must retain explicit dispositions.

Final acceptance additionally requires the final signed artifacts across core,
shell, C API/ADBC, Go, Swift, platform packages, cross-engine storage/WAL and native
coupled TPC-H/ClickBench speed/RSS with per-query outliers and documented performance
acceptance. The lower-memory policy and accepted memory-bound COPY exception remain
unchanged. Mac Intel and additional language clients remain outside scope.
