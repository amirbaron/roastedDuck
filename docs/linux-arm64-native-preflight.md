# Linux arm64 native checkpoint — 2026-10-30

This checkpoint predates the newer
[integer-average precision candidate](integer-average-precision-parity.md).
Its artifact hashes and155-case C API result remain controls; the updated engine
still needs optimized native and complete final verification.

This is a platform checkpoint, not final release or whole-port acceptance.
The optimized signed Linux arm64 candidate passes all30 upstream C API files:
**155 cases /862,010 assertions**, with zero failures. Runtime container swap is
disabled. Signed manifest and payload hashes verify before and after the suite.
Source hashes remain unchanged across the run.

The pinned manylinux2.28 arm64 environment runs Rust1.99.0 with the unchanged
full-LTO release profile. It also passes42 signed debug SDK tests and eight
optimized native smoke checks: CLI version,UTF-8/nested CSV values,transaction/
checkpoint/reopen,C ABI compile/load/query/close,547 C API exports with no ADBC test
hooks,SQL-runner aggregate/rollback,dynamic dependencies and signed integrity.
These smoke checks are not the full SQL/shell/ADBC/client/storage/WAL gates.

## Artifact identities

| Artifact | SHA256 |
|---|---|
| libduckdb.so | `92805a088fc259dd5d9c35a0ae4068633c6f1f43e226979fba1709172b794687` |
| libduckdb.a | `deb96ed20eb4d749bd347ab4534b4adbc36719f76fb1089dd58b03e982f97842` |
| rustdb | `dac4372e52fac2f9c07e7848bc9aa57d3e343f7554da2a3188fbb6cee26543fb` |
| slt | `5166bd3b3ad401dac3d2fa2186942ac271612537fccb8ccfe414488ac6af54c1` |

Detached content signatures use the existing local `rustdb-dev` identity and the
portable verifier. The certificate fingerprint comes from an independently kept
host signing record. This establishes content authentication; it is not a claim
of platform vendor signing,Apple notarization or a published production release.

## Preserved failures and changes

The first optimized build ended with rustc SIGKILL/exit101 while linking the shell;
Docker records `OOMKilled=true`. The isolated2CPU/8GiB VM had no swap. The retry
keeps the same source/profile and adds8GiB compiler swap on its SSD-backed data
disk. It completes in6m19. Runtime tests cap RAM+swap at RAM, disabling swap.
Compiler capacity is not a runtime performance acceptance result.

The first native smoke fixture assumed unquoted CSV strings. Actual values were
correct; the retained second attempt parses CSV for the typed-value check. Final
shell byte comparison remains separately required.

The first full C API attempt passes29 files; the appender file fails to compile
because the shared test shim implicitly relied on an `<atomic>` include supplied
by libc++. Adding the explicit standard header makes the complete GCC run pass.
No assertions or test cases are removed.

Harness portability changes select `.so`/`.dylib`, use GNU section collection and
pthread flags on Linux, preserve Darwin flags and support external C API output
directories. Empty flag arrays retain compatibility with macOS Bash3.2. The ADBC
launcher also selects the correct suffix and Linux dl/pthread flags; that path has
not yet been executed on Linux. All compiled engine inputs are unchanged.

## Evidence and remaining gates

Evidence root: `.context/artifacts/platform-matrix-20261029/`, resolving to the
workspace's dedicated external SSD directory. Relevant records:

- `source-{1,2,3}/manifest.json`: compiled engine identity and separate harness edits.
- `linux-arm64-native-build-{1,2}/`: failed and successful build terminals/logs.
- `linux-build-swap-1.json`: isolated compiler-capacity setup.
- `linux-native-freeze-1.json`: trusted artifact hashes and signing record.
- `work/linux-arm64/native-release-signed-1/`: four frozen artifacts and manifest.
- `work/linux-arm64/native-runtime-2/verification.json`: eight smoke checks.
- `linux-arm64-capi-{1,2}/`: both complete harness attempts and terminal results.
- `work/linux-arm64/capi-2/verification.json`:30 files/155 cases/862,010 assertions.

The Linux C++1.5.6 comparator builds from a fresh SSD extraction of the supplied
source archive, mounted read-only. Its shared library,shell,upstream test runner and
oracle shim are frozen and signature-verified. Its git identifier is explicitly
synthetic because the archive has no git history; the archive SHA256 is recorded
separately. The first freezer expected the CLI under `tools/shell`; the corrected
attempt uses CMake's actual build-root executable and preserves the partial attempt.

The first paired corpus diagnostic proves selection of the frozen C++ library but
finds missing loadable-demo test extensions: selecting the engine/shell/unittest
targets had omitted the remaining CMake assets. The owned diagnostic container is
stopped and its partial output retained; it is not acceptance evidence. CMake ALL
targets now complete with the original engine hash unchanged. All23 runtime assets
are signed and verified. The second full paired run is active with a fresh fixture
matching all4,870 SQL files and1,522 data entries and a read-only reference build.
Both previously missing-demo failures now pass on C++; full results remain pending.
No Linux C++ paired parity result is claimed yet.

The active control run records one C++ worker OOM in the unchanged4.3GB gzip
round-trip test. Filtered kernel evidence identifies the owned worker and
`CONSTRAINT_MEMCG` at the7GiB container cap with swap disabled. The full run
continues; a separate unchanged-SQL correctness follow-up is prepared with an
explicit SSD-swap allowance and separate per-engine fixtures. It has not run and
will not establish performance acceptance or change the Rust memory policy.
Evidence: `linux-paired-corpus-2-{resource-probe,kernel-oom}-1.json` and the original
worker log. This control failure has no accepted final disposition yet.

The C++ VARIANT TPC-H SF1 storage file also reaches the unchanged300-second limit
while its worker is actively using CPU. Its canonical timeout is retained. A
separate exact-SQL follow-up is prepared with the same7GiB/no-swap limits and a
900-second bound; it has not run. Neither control exception establishes a Rust
regression or a completed parity gate.

The Go harness now selects native macOS/Linux and arm64/amd64, checks the library
architecture, records Linux rpath/loader selection and requires a caller-supplied
certificate pin plus signed manifest before Linux execution. Actual Linux helper
preflights verify the signed library, reject an unlisted manifest artifact and
resolve the selected library through a path containing spaces. Evidence is under
`go-portable-helper-preflight-{1,2,3}/`; trust/membership rejections also pass under
Python optimization. The native Go1.27.3 Linux arm64 toolchain and all79 pinned
upstream source files are verified and prepared on SSD. These checks do not establish Go compilation,
the652-case suite or consumer/WAL acceptance. The complete Go build/test and Mac
regression verification remain pending. Engine inputs are unchanged.

Remaining: full SQL/shell,ADBC,Python/Go/Swift,cross-engine persistence/WAL and coupled
speed/RSS gates on final signed artifacts; Linux x86_64,macOS x86_64 and iOS slices;
final integrated Mac refresh. Other language clients and Apple OS families remain
outside scope. The accepted lower-memory performance policy is unchanged.
