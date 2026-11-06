# C API validity and streaming repairs

The first signed Linux x86_64 C API run exposed three NULL-handling failures and
a streaming stall. The C++ control passed all 155 cases. Original Rust failures,
test executables, library bytes and logs remain preserved under external
`.context/artifacts/platform-matrix-20261029/work/linux-amd64/capi-paired-1/`.

The aggregate callback counted NULL input as valid, the variadic scalar callback
counted no NULLs and aborted on a C++ assertion, and the data-chunk reader treated
a NULL VARCHAR slot as a valid empty string. An independent caller-owned bitmap
control isolated the shared cause: the optimized exported validity function
returned shifted word bytes such as 2 or 254 for a false bit. GCC and libffi
callers tested the whole byte. The bitmap itself was correct.

The C-facing symbol now passes through an explicit zero-or-one byte boundary.
The Rust-facing function signature, bitmap semantics and `duckdb.h` are unchanged.
Disassembly and 35 bitmap checks per engine confirm canonical bytes and values
on the fixed signed candidate. This is a targeted interoperability repair;
there is no table-cache or memory-budget change.

The streaming test stalled for 689 seconds with the query and worker threads
waiting on futexes. Only that verified owned test child was terminated, allowing
the failed collector to finish. A deterministic bounded regression then showed
that an already-ready stream waited for another notification. The test's failure
path wakes, joins and cancels the producer instead of hanging the suite.

The executor's wait predicate now recognizes already-blocked work. A collector
notification arriving between the caller's poll and generation snapshot no longer
leaves the consumer asleep waiting for a later notification. The before-fix test
fails; the signed after-fix test passes, and all 13 affected executor unit tests
pass with no ignored tests. This fixes the condition rather than adding polling
or increasing the streaming buffer.

The optimized source-41 Linux x86_64 candidate passes the original 30-file,
155-case C API suite with 943,838 assertions. The original signed GCC executables
were reused without modification; each loader check selects the new library.
All 547 C API exports remain, and candidate/harness signatures authenticate
before and after execution. Its DLL SHA256 is
`7b2bdd36c2b054e73b4ba8200e65e05066b12c9e49d4f23cb0772aae4eaa7453`.

Formatting and strict Clippy for C API/execution all targets pass after a
documentation-only backtick correction in an existing instance-cache test.
Source-42 records that correction; product code is identical to the verified
source-41 candidate. Required Apple/Linux arm64 artifacts and selected clients
still need refreshed verification, and this is not final release acceptance.

Evidence under the external artifact task:

- `linux-amd64-null-stream-behavior-verification-41.json`: aggregate repair evidence.
- `work/linux-amd64/capi-null-stream-signed-41/`: immutable signed DLL/static archive.
- `work/linux-amd64/capi-null-stream-rerun-41/verification.json`: full C API result.
- `work/linux-amd64/capi-validity-bits-{2,41}/`: original/fixed bitmap controls and disassembly.
- `linux-amd64-stream-{before-run-40,after-run-41}/`: bounded failing/passing regression.
- `linux-amd64-execution-full-run-41/`: all 13 affected unit tests.
- `linux-amd64-capi-streaming-stall-1.json`: original stall and exact child termination.
- `linux-amd64-null-stream-check-{41,42}/`: original documentation warning and successful checks.

The source-39 canonical SQL run remains historical component evidence. Its shared
gzip resource failures, failed one-file ratchet, reference VARIANT timeout and
individual timing outliers are not rewritten by these repairs. Final common-source
SQL, shell, ADBC, Python, Go, Swift, storage/WAL and coupled speed/RSS acceptance
remain required.
