# Linux x86_64 preparation

The latest signed source-46 library passes all 155 C API cases and five live
C++ process-memory controls. Both engines pass 392 upstream shell tests with
three identical skips. The full differential remains a separate open gate;
see [shell diagnostics](linux-shell-parity.md) and
[process memory](linux-process-memory-parity.md). Older artifact hashes and
source-39 corpus results below are preserved component evidence.

The dedicated SSD-backed Linux VM executes x86_64 ELF files through Rosetta.
The guest kernel also runs aarch64 containers; the amd64 process maps explicitly
contain `/mnt/lima-rosetta/rosetta`. This is translated functional-test execution,
not native x86_64 hardware or performance acceptance. Other VM profiles and the
host's active Docker context remain unchanged.

The builder is pinned as
`quay.io/pypa/manylinux_2_28_x86_64@sha256:e0adf18521ad35192df733a6c62d35df7801957d24e0470238aacecfab2188a3`.
Its registry descriptor and downloaded image both identify linux/amd64. An actual
container probe reports x86_64, ELF machine62 and glibc2.28. GCC's builtin macros
report eight-byte pointers and a64-bit `long double` mantissa, matching the Intel
precision branch rather than the113-bit Linux arm64 branch.

Evidence is under `.context/artifacts/platform-matrix-20261029/`:

- `linux-amd64-preflight-1/`: original descriptor, ARM guest check and actual amd64
  execution/ABI/translator probe.
- `manylinux-amd64-image-1/`: exact pinned image identity and successful pull.

Rust 1.99.0 installs with the verified official installer checksum and reports
host `x86_64-unknown-linux-gnu`, LLVM 23.1.1. Locked compiler checks pass for C API,
shell and SQL runner with all 1,004 guarded source hashes unchanged. Eight debug
SDK executables are frozen as ELF x86_64, signed by a host-pinned content manifest,
then run with runtime swap disabled: all 49 tests pass, with zero ignored/failures.
Signatures verify before/after execution. Evidence: `linux-amd64-check-1/`,
`linux-amd64-sdk-freeze-1.json`, `linux-amd64-sdk-run-1/` and
`work/linux-amd64/sdk-runtime-1/verification.json`.

The optimized core, static archive, CLI and SQL runner now build successfully
with full release LTO and unchanged source39 guards. Frozen ELF x86_64 payloads
are authenticated by the host-pinned detached signature before execution.
The signed optimized preflight passes CLI version/UTF-8/list/struct values,
transaction rollback/checkpoint/reopen, public ctypes C API open/connect/query/
close,547 exports/no ADBC hooks, SQL-runner aggregate/transaction checks and dynamic
dependency resolution. Runtime swap is disabled and signatures verify afterward.

The shipping candidate library SHA256 is
`4c963328199cf43cac0ecb1c21f5ad7ff6c9cdefd3103dc51d9960a5d253d7dd`.
Evidence: `linux-amd64-native-freeze-2.json`,
`work/linux-amd64/native-release-signed-2/` and
`work/linux-amd64/native-runtime-1/verification.json`. The first freeze attempt
stopped before signing because it reused an existing ARM log filename; that
partial copy is retained, and the successful attempt uses fresh AMD64 paths.

The supplied C++1.5.6 archive is pinned at
`1fadcbe9e69e1470f9093b6bcde08daf477d729c449e59a807f45c346622099b`.
Its full serial comparator/assets/oracle build completes successfully under
`linux-amd64-cpp-build-1/`. Four frozen ELF x86_64 artifacts and all 16 loadable
extension modules authenticate through the host-pinned signatures. The C++ library
SHA256 is `7a0d906abac7f6e6a4d72bff86777292e5ba48bb24895f90cb0212ca45e70746`.
The complete paired4,870-file corpus uses one worker, normal engine
threads and a300-second bound. All1,522 data/SQL input hashes and all three
signature groups verify before execution, and `ldd` resolves the exact frozen
C++ oracle library. Evidence: `linux-amd64-canonical-setup-verification-1.json` and
`work/linux-amd64/paired-corpus-1/`.

The C++ reference completes all4,870 files:4,793 pass,75 skip, one gzip worker is
OOM-killed and one VARIANT TPC-H SF1 case reaches the300-second timeout. Original
results are retained; neither nonpassing result is accepted automatically. The
fixture postcheck finds no changed/missing/extra SQL or data inputs, and all three
native signature groups reauthenticate (7Rust,7C++ and23reference-asset payloads).
`linux-amd64-cpp-corpus-completion-1.json` records the complete reference result and
actual integrity checks. Separate original failure receipts are
`linux-amd64-gzip-canonical-oom-1.json` and
`linux-amd64-variant-tpch-canonical-timeout-1.json`; the latter has no new OOM event.

Rust also completes all4,870 files:4,779 pass,75 skip,15 failures match the existing
documented exception details, and one gzip worker is OOM-killed. All75 skip reasons
match C++ literally. The result classification finds no other unexplained Rust
nonpasses; the shared gzip failure is retained separately from the15 exceptions.
All SQL/data fixtures and all three signed payload groups verify after both engines.

The exact signed SQL runner reports4855 listed,one regression,zero newly passing:
the unchanged canonical ratchet **fails on the shared gzip kill**. It is not
rewritten as green. `linux-amd64-canonical-review-1.json`, its companion Markdown
report, and `linux-amd64-canonical-ratchet-1/` retain the complete classification,
literal skip comparisons, original failures and ratchet terminal. Final acceptance
still requires the separate resource controls and remaining release gates.
The original Rust VARIANT TPC-H SF1 case passes in179,872ms under the unchanged
300-second bound. `linux-amd64-rust-variant-tpch-canonical-pass-1.json` retains
that result alongside the original C++ timeout; the timeout is not rewritten as
a pass or used as a completed C++ timing comparison.
Its canonical large-gzip worker is also OOM-killed (3,111ms, empty worker log).
The cgroup OOM-kill count rises from one to two, independently confirming the new
Rust event rather than relying on Docker's latched OOM flag. Both original
nonpassing outcomes remain preserved; an unchanged-input resource follow-up is
still required. `linux-amd64-rust-gzip-canonical-oom-1.json` records the Rust event.

One completed Rust case is an explicit open timing outlier:
`test/parquet/variant/variant_roundtrip.test_slow` passes in151,767ms versus
C++21,889ms,6.93times longer for the whole file. This is translated execution and
has no paired per-query RSS measurement; it is not accepted through the overall
lower-memory tradeoff. The original result is retained in
`linux-amd64-parquet-variant-timing-outlier-1.json`. A static audit of the exact
candidate-source39 adapters finds buffer-managed materialized queries and separate
render methods on both sides, not an established eager-Rust/lazy-C++ formatting
explanation. `linux-amd64-parquet-variant-static-audit-1.json` records that limited
finding. Isolated query/fixture profiling and coupled speed/RSS disposition remain
required; increasing memory or restoring table caching is not the default remedy.

The complete diagnostic report lists21 whole-file timing outliers using positive
C++time, Rusttime at least1second and a Rust/C++ ratio of at least2. The largest is
`test/sql/join/test_hash_join_many_columns.test_slow`:34,027ms versus1,673ms,
20.339times longer. All qualifying cases are listed individually in
`linux-amd64-canonical-review-1.md`. These translated whole-file timings have no
paired per-query RSS and do not establish an accepted speed/memory tradeoff. The
earlier live-worker attribution to JSONTPC-H was corrected: the completed case
passes in936ms versusC++1867ms and is not a timing outlier.

Full paired SQL/shell/C API/ADBC/client tests, cross-engine persistence/WAL and coupled
performance acceptance remain pending; a completed canonical execution with a
failed resource-profile ratchet is not final C++ parity. The initial full C API
run found NULL-validity ABI failures and a streaming stall. The targeted signed
source-41 repairs now pass all155 C API cases and13 affected executor unit tests;
see [the before/after repair evidence](capi-null-stream-parity.md). Other required
platform/client gates and final common-source verification remain open.
New toolchains, targets, downloads and logs
must use separate directories on the SSD. Only one heavy workload runs at a time.
