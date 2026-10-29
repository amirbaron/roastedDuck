# FSST encoder and persistent-storage parity

Updated2026-10-29. The confirmed embedded-NUL encoder discrepancy is fixed and
verified on signed native images. The whole port goal remains active; final full
corpus, shell, selected-client, ADBC, persistence/WAL and performance gates remain.
Python, Go and Swift are the only selected clients.

## Counterexample and correction

The live queue's historical5878-versus5877 segment-count observation has no retained
exact input. Frozen C++ storage analysis samples vectors using a randomly seeded
RandomEngine; Rust uses a fixed seed. Segment/table identity therefore needs
controlled sampling. A different stored table alone does not prove incompatibility.
The historical density observation is not a calibrated remaining correctness gap.

Independent signed tools compiled from DuckDB1.5.6's vendored FSST and the actual
Rust primitive bypass that outer sampling. Initially8/9 corpora had identical
exported tables and compressed records; embedded-NUL length strings differed.
C++ produced7symbols versus Rust8, both40-byte tables. All18 cross-decodes still
passed, so round-trip tests alone concealed this encoder discrepancy.

A trace-only C++ oracle retained its original compressed output exactly. The first
queue difference had equal gain28 and symbol value0x61, with lengths7 and4.
C++ queue priority compares gain/value, while Rust added a length tie-break.
C++ also erases and reinserts an accumulated candidate into its unordered_set.
The correction reproduces that candidate lifecycle, the frozen QSymbol hash and
libc++ push_heap/Floyd-pop_heap tie handling. Common UnorderedOrder now supports
erasure, counts live keys for growth, and reuses erased slots. Existing insertion
and configuration-copy order controls still pass. No table cache was introduced.

## Current-source verification

Source3 contains946 overlay entries/945 build inputs; all guards pass before and
after execution. All native executables and libraries are signed before execution.
The C++ container fixtures use Apple clang21/libc++ with the macOS27 SDK; exact
encoder byte comparisons are calibrated to this reference platform. Storage/WAL
compatibility is tested through public C ABI processes independently.

| Gate | Result |
|---|---|
| Workspace/all-target Clippy, warnings denied | Pass |
| Signed common/storage debug and release |124/124 each,0ignored |
| Independent container/heap regressions |899 native erase/reinsert/collision/growth operations and512 queue removals |
| Exact same-training-input encoder controls |73/73 identical tables and compressed records;146 opposite-engine decodes |
| Signed shipping build | Actual parent exit0; frozen CLI/library/SQL runner;547 C API exports, zero ADBC test hooks |
| Affected SQL corpus |190 files:186pass, four exact C++ skips; no crashes/timeouts or verdict regressions |
| Full C API suite |30files/155cases/797774assertions pass; each loader selects the immutable current library |
| FSST/DICT_FSST persistence |12 bidirectional checkpoint/nonempty-crash-WAL/torn-tail journeys; receiving-engine mutations/checkpoint and original-engine final readback all pass |

Persistence checks use full EXCEPT ALL comparisons, cross-engine count/length/hash
summaries, index fetches and filtered reads of NULL, empty, Unicode, long and NUL
strings. They avoid C-string truncation when checking values. FSST fixtures pin
storagev1.1.0; DICT_FSST fixtures pinv1.3.0. The first unpinned DICT_FSST fixture
selected Uncompressed in C++'s defaultv1.0 format; that harness calibration failure
is preserved. Four SQL skips are the unchanged block-size prerequisite and three
HTTPFS prerequisites, with exact C++ reasons.

The final storage run preserves20 immutable database/WAL input files,23,538,204
bytes total, including actual nonempty crash-retained WAL and three-byte truncated
trailing headers. Their hashes are verified after receiver mutation/readback.
The earlier compatible-but-different encoder output and all failed tool/build/
Clippy/fixture attempts remain separate; no expectation or ratchet was weakened.

Current library SHA256:
`eda5ab536f9feb10bcfd2ac7279b64f1d9e9ff19d200a378d4ed3c4ffef5d960`.
CLI: `2044ecd3f114f72e4f4eb4f156964ec7c73bacf8cba80b91243d7b4ce715f7f5`.
SQL runner: `0906e5e05652ce047247f296c0b1bebdae319b85afd52fb6fc34b32866dad6dd`.

External evidence: `.context/artifacts/fsst-parity-20261029/`, including
`source-3/manifest.json`, `primitive-verification.json`, `inner-{4,5}/`,
`golden-provenance.json`, `tests-debug-2/verification.json`,
`tests-release-1/verification.json`, `clippy-2-terminal.json`,
`native-images-1.json`, `affected-sql-1/verification.json`,
`capi-suite-1/verification.json`, `engine-3/verification.json` and `engine-3/inputs/`.
Initial counterexample and trace outputs remain in `inner-1/`, `ref-queue-1.log`
and `rust-queue-1.log`. The artifact link points to the dedicated external SSD.

This component checkpoint does not establish final TPC-H/ClickBench acceptance.
Preserve the accepted lower-memory policy, the explicit COPY tradeoff and individual
query outliers. The next confirmed core gap is aggregate callback equality.
