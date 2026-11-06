# Required release platforms

Current disposition, November 6, 2026: the owner explicitly approved a physical-device test exception. The goal is complete under that scope: all 22 scoped software collection/review requirements are evidenced; actual iPhone/iPad execution and provisioning remain **unverified, excepted by the owner**. Device builds, signatures and simulator tests retain their verified status. The [delivery index](release-candidate-index-20261106.md) links the new signed disposition, unchanged archive and all known limitations. Older dated statuses below are historical.

The exact Mac2556 and Linux2663 engine images have completed the recorded SQL, shell, C API/ADBC, Python, Go, Swift, storage and codec checks. All 15 normal Python wheels are qualified. Final relocation/runtime audit2921 passed, and the signed candidate archive2924 extracted with all 300 payload files and symlink targets unchanged. Known SQL failures, matching skips, reference crashes and shell differences remain explicit; this is scoped verification, not a universal no-exception claim.

The unchanged aggregate performance policy is met: best-time geometric means are 7.2–15.6% longer, with 62.2–78.1% less interpreter-adjusted geometric-mean RSS. ClickBench summed times remain 22–25% longer, and its largest one-thread peak is almost C++'s. The [current performance report](performance-final-native-20261106.md) retains all 63 flagged configurations, four COPY profiles, five whole-file stress outliers and every raw sample. No cache or memory budget was increased.

The live worktree matches 2,439 of the frozen snapshot's 2,446 inputs byte for byte. The seven differences are status Markdown files; no Rust, Cargo, runtime, test or fixture inputs differ, and no additional source files were found. The tested snapshot and raw records remain unchanged.

| Required scope | Verified evidence | Remaining boundary |
|---|---|---|
| macOS arm64 | Full SQL/shell/workspace collections; C API/ADBC, Go, Swift and five Python ABIs; storage/codecs, native benchmarks, final relocated clients and CLI. | Recorded exceptions remain. Intel Mac is outside the goal. |
| Linux arm64 and x86_64 | Full SQL/shell collections; C API/ADBC, Go, Swift, ten Python ABIs, storage/codecs and final native-path API/ADBC/CLI probes. | x86_64 functionality uses translation; native x86_64 performance is not claimed. |
| iOS arm64 device and simulator | Three signed ARM-only SDK slices; 133 actual simulator Swift cases, typed consumer and four cross-engine WAL journeys on unchanged SDK bytes. | Physical-device execution/provisioning and runtime identity remain unverified, with no waiver. |
| Performance | 130 checked native configurations, independent numeric/full tied-result controls, unchanged aggregate acceptance policy and individual outlier review. | No exact per-query C++ performance or outperformance guarantee. |
| Signed distribution | 24 roles, 15 wheels, three ARM slices, 300 unchanged files, CMS signatures, relocation/runtime checks and archive extraction. | Full goal completion still requires the physical iOS gate or an explicit owner exception. |

Owner-confirmed on2026-10-29: **Apple platforms plus Linux**, then clarified as
**macOS and iOS devices/simulators**. tvOS,watchOS,visionOS and Windows are outside
this release goal. Language clients remain Python,Go,Swift; preserve C API/ADBC.

Owner update on 2026-10-30: **Mac Intel support is no longer required**. The
required Mac target is arm64; prior Intel checkpoints remain historical evidence.
The required simulator runtime target follows Apple silicon (arm64); the already
compiled Intel simulator slice remains optional compatibility evidence. Linux
x86_64 and arm64 remain required. The owner also permits internal storage for iOS
simulators and other work blocked on the SSD; evidence remains on the SSD.

## Historical source2078 artifact frontier (retired)

Recovery update (2026-11-04): the VM interruption preserved in evidence 2237 stopped the first x86_64 Rust collection at 2,825 cases. Fresh complete collection 2238 is running; completed C++ records and all prior verified Mac/ARM components are retained with their original bytes and scope. The table below states those verified components; an interrupted partial run never substitutes for a full gate.

| Required scope | Verified current evidence | Still required |
|---|---|---|
| macOS arm64 SQL/core | Both complete 4,870-file collections reviewed in 2093: Rust 4,826 passes, 30 shared skips and 14 documented raw failures; C++ 4,839 passes, 30 skips and one independently diagnosed reference failure. Exact 110,464 CAST/TRY_CAST queries and six focused cohorts match. Workspace review 2191 verifies 1,083 passes, zero failures and seven declared ignores across 149 harnesses. | Required ignored functional supplements, full shell and storage/WAL refresh, coherent artifact audit. All raw exceptions remain retained. |
| macOS C API/ADBC | C API 2094/2187: 30 harnesses, 155 cases, 705,024 actual assertions. ADBC 2095/2190: 64 cases and 3,040 assertions per engine. Shipping retains 547 exports and zero private test hooks. | Linux API suites and final artifact acceptance. |
| macOS Python | Matrix 2104/2197 verifies all five fresh CPython 3.10–3.14 normal-ABI cells: 31,135 matching XML records per engine, 36 typed WAL journeys, 216 encrypted-Parquet cases, 18 installed ADBC environments, 40 typing runs and 108 coupled client samples. | Ten Linux ABI cells and final performance acceptance; individual client outliers remain separate. |
| macOS Go/Swift | Review 2198 verifies Go's 652 cases per engine, four independent typed WAL journeys and Swift's 133 cases per engine. SDK review 2126/2199 verifies relocation, two normal consumers and four further typed WAL journeys. | Linux Go/Swift suites and consumer journeys. |
| iOS device/simulator | SDK 2114 has three signed ARM slices with 547 exports each. Review 2126/2199 verifies all 133 actual simulator Swift cases, the typed consumer and four cross-engine WAL journeys. | Physical-device runtime and provisioning remain unavailable, unverified and required. Simulator success does not substitute for hardware evidence. |
| Linux ARM64/x86_64 | Both signed 2080 builds have 547 exports and zero private test hooks. ARM review 2212 verifies all 9,740 records: C++ 4,842 passes/28 skips; Rust 4,828 passes/28 skips/14 documented raw failures, with no unknown nonpasses or resource kills. All original 40-GB, concurrent-index and Linux extended-precision cases pass on both ARM engines. | Complete x86_64 pair and independent review; both architectures' shell, C API/ADBC, Python, Go, Swift and storage/WAL suites. Recovery 2215 has passed the prior Rosetta stop and the C++ 40-GB case; it remains incomplete. |
| Coupled performance | The lower-memory policy is unchanged. Current Python client measurements and every SQL-file timing outlier are retained separately. Current 130-configuration macro, COPY calibration and coupled SQL-file outlier measurements are prepared. | Actual final TPC-H/ClickBench answers, best/median weighted and geometric-mean times, whole/adjusted peak RSS, individual outlier review and documented acceptance. Prepared work is not performance acceptance. |

Recovery evidence 2213 preserves the x86_64 reference run's actual exit 133 and
Rosetta temporary-file assertion. Docker recorded no OOM kill. Fresh observation
found the translated monitor accumulating `/proc/<pid>/auxv` descriptors while
SLT remained at nine. Recovery 2215 raises only the inherited descriptor limit
from 1,024 to 65,536; engine memory/cache policy, signed binaries and tests are
unchanged. Serial dispatcher 2222 resumes unstarted components after the recovered
corpus succeeds. Independent review 2223 is prepared separately. The goal remains
incomplete; [current verification](current-source-verification.md) and
`.context/corpus-resume-state.json` contain live evidence pointers.

The x86_64 retry has recorded one unresolved C++ reference worker crash in
`test/sql/cast/float_decimal_cast.test`. Its worker log is empty; the resource
guard recorded no kill and observed cgroup OOM/max counters remain zero. The
same original case passed on native ARM64. Evidence 2226 retains the raw crash;
it is not an approved exception and does not count as a pass. The full collection
continues so the independent review can assess every actual result.

Current checkpoint 2235 closes the disk-prerequisite coverage gap: the unchanged
`require disk_space 40gb` case now actually passes in all four current Linux
C++/Rust collections. Original skips remain preserved. The x86_64 Rust collection
also passes the exact original float/decimal rounding case that crashed in C++;
this does not erase or approve the reference crash. The whole Rust collection and
the queued scoped reference controls remain incomplete.

## Historical source 1959 frontier (retired after the index-publication bug)

Current source1959/core1961 includes the corrected S3 upload context. Older
source1634/core1637 results below remain historical. All required target/client
combinations remain in scope; device and simulator evidence are separate.

| Required scope | Current candidate evidence | Still required |
|---|---|---|
| macOS arm64 core | Both full4,870-file SQL collections reviewed2022; Rust4,826pass/30skip/14documented raw failures; C++4,838pass/30skip/two reference failures. All76nonpasses reviewed. Exact110,464CAST queries match; six focused controls match1,893records per engine. | Current full shell/storage/WAL and final coherent artifact audit; every exception remains explicit. |
| macOS arm64 C API/ADBC | C API1971:30harnesses/155cases/745,460assertions. ADBC1972:64cases/3,040assertions per engine; unchanged547-export/zero-hook shipping image. | Other applicable targets and remaining final release gates. |
| macOS Python | All five CPython3.10–3.14 cells and independent1980 reviews complete;2024exits0. Aggregate31,135matching records per engine,36typed WAL journeys and108coupled client samples. | Linux Python cells and final macro/release acceptance; individual client timings remain separate. |
| macOS Go/Swift | Go1982 passes652cases per engine and1984 verifies four independent typed WAL journeys. Swift1985 passes133cases per engine;1991/2001 verify SDK relocation/two normal consumers/four typed WAL journeys. | Linux Go/Swift and remaining final release gates. |
| iOS arm64 device/simulator | Both current iOS ARM slices build/sign, three-slice SDK has547Cexports each. Full133Swift cases, typed simulator consumer and four cross-engine WAL journeys pass; independent2001 reviews actual images/signatures/case IDs. Main-disk runtime is used. | Physical-device execution/provisioning is unavailable, unverified and not waived; remaining all-platform release gates stay open. |
| Linux arm64/x86_64 | Both ARM2003/AMD2004 builds and2005signed freezes pass547exports/zero private hooks. ARM full SQL2010 is live under corpus2068; shell2029/API2030/GoSwift2031 remain queued. | Both architectures’ runtime gates, all399original filenames on native storage, independent SQL nonpass disposition; ten Python cells/storage in phaseB2047. |
| Coupled performance | CP310–312 client measurements recorded individually; native130configuration macro harness2055 prepared. Memory/cache policy unchanged. | Actual current TPC-H/ClickBench answers, paired speed/RSS, weighted best/median totals, maximum RSS, every individual outlier and documented acceptance. |

Dispatcher2032 stopped on a CWD-only source-verifier preflight failure before
iOS compilation. Recovery2063 passes from both working directories with unchanged
production source and crypto/native pins. Dispatcher2065/session65225 completed Apple2064 and both Linux builds2027, then
stopped on a stale verifier pin before corpus payloads. Recovery2067 checks both
preparation-map formats and preserves completed gates/failure receipts. Current
dispatcher2069/session59928 runs ARM full SQL collection as the sole heavy workload. The goal is incomplete;
[current verification](current-source-verification.md) and
`.context/corpus-resume-state.json` identify live state and external-SSD receipts.
Historical success on other native bytes does not close current-artifact gates.

## Historical artifact frontier (source1634/1742; superseded)

The current common core is signed macOS candidate1637/source1634, with corrected
Python binding source1742/wheel1744. Equivalence1749 checks that only the Python
leaf changed. Historical target evidence below does not establish acceptance for
these artifacts.

| Required scope | Current evidence | Still required |
|---|---|---|
| macOS arm64 core | Both fresh4,870-file SQL collections reviewed; exact110,464cast queries per engine match; shipping HTTPJSON1713 matches; shell1796 reviewed;20storage/WAL journeys and40tail recoveries pass | Current SDK gates and399native-Linux filename fixtures; documented shell exceptions retained |
| macOS arm64 C API/ADBC | C API1661:30harnesses/155cases; native ADBC1662:64cases/3,040assertions per engine; shipping library unchanged with no test hooks | All other applicable targets; standalone/public consumers remain separately scoped |
| macOS arm64 Go/Swift | Go1668:652cases each/fourWALjourneys1670; Swift1671:133each; currentSDK1774relocation/standalone/two normal/fourtypedWAL pass, review1848 | Linux Go/Swift and remaining final all-platform acceptance |
| macOS CPython3.10–3.14 | All5declared normal ABI cells and independent reviews1816/1822/1831pass:31,135matching XML records per engine,36typed WAL journeys,216encryption cases,18installed ADBC environments and108client samples | Individual client timing outliers and reference-only warnings retained; Linux cells and final macro/release acceptance remain open |
| iOS arm64 device/simulator | Current3ARM SDK build/sign verified; full133Swift cases and typed consumer/four cross-engine WAL journeys pass on actual arm64simulator, independently reviewed1848 | Physical-device execution/provisioning remains unverified; other final release gates remain open |
| Linux arm64/x86_64 | Current ARM1838release build/sign1812passes547Cexports/zero test hooks; AMD1839building. Exact153locked crate archives per architecture verified; ARMHTTPFSreference authenticated | Runtime/native399filename checks1846, focusedcontrols1847, recorded correctness capacity/freshoracle/full4870Cpp+Rust corpora1851; all C API/ADBC/PythonGoSwift/storage/WAL and final acceptance |
| Final coupled performance | Four CP310 client workload observations1758 documented individually | Native current-core130 TPC-H/ClickBench query/thread configurations, all answers, paired speed/RSS, weighted totals/maxRSS, individual outliers and documented acceptance |

Native Linux filename follow-up1846 is queued after1821build/sign completion. It
runs the unchanged exhaustive8,745argv and2,120dot-path cases on each target's
native guest filesystem, with exact signed artifacts/loader checks. The original
266argv+133dot-path Mac fixture-error case identities, raw argv and stdin bytes
are retained separately and must all execute/match. Working files remain inside
owned stopped containers on the SSD-backed VM; reports export to the SSD artifact
bind. This is prepared work, not completed Linux evidence; other shell/SQL/client/
storage/WAL and final performance gates remain open.

Linux arm64 HTTPFS reference acquisition1832/recovery1833now verifies the official
1.5.6ELF package (`ae940d800b7f938a4848d0eebcffb28dda0bbed76987f11dc55faa6d1a86db0d`).
Independent OpenSSL validation uses the exact20core RSA keys shared by the pinned
Rust/C++sources; one-bit digest mutation is rejected, and local pinned CMS passes.
The downloaded module has not executed and remains outside Rust shipping artifacts.
The initial observer's incorrect18-key count is retained as a failed attempt;
recovery changes that count to the actual identical20-key source lists. Linux
engine LOAD/full SQL/API/client/storage/WAL acceptance is still required.

Read-only device discovery1776/1777 and fresh1829reports12simulated devices and **zero physical
devices**. Connected CoreDevice tunnels must not be described as physical iOS
hardware. The dedicated simulator and its main-disk runtime directory are separate
from other workspaces' devices. No device was booted, paired or provisioned by that
probe. Keep physical-device, simulator and compile-only claims distinct.

The normal workspace run1765 passes149harnesses/1,081tests with seven declared
ignores. Review1790 confirms their IDs match prior1412:five diagnostic tools remain not passed;
two functional supplements now execute:1785 cast implemented/flat cases match;1802 full RE2 pattern inventory passes. Unit ICU diagnostics stay raw; signed SQL1639 has zero differences. The full goal is
incomplete. Live process and receipt references are in
`.context/corpus-resume-state.json`; see
[current-source verification](current-source-verification.md).

Current Linux ARM argv/path review1879now covers all10,865original generated cases:
argv8,079exact/666raw native-stack mismatches; dot-path2,120exact. All399original
Mac-unrepresentable filename cases execute and match on native guest storage.
Every mismatch has exact literal error preamble, exit code, other stream and file
effects; the C++native frame suffix remains a raw nonpass. Original case/argv/stdin
bytes, generator hashes, signed CLI hashes and manifests verify, with zero
unexplained functional differences. The prepared collector's10685scope-text typo
is corrected here to the actual8,745+2,120=10,865cases; its per-suite assertions and
coverage never changed. x86_64collection is still running. This closes only the
ARM native-filename/argv/path obligation, not the remaining full shell profiles,
core/client/storage/WAL or final performance gates.

## Historical platform checkpoints

| Platform | Planned architectures | Applicable artifacts and clients | Current evidence |
|---|---|---|---|
| macOS |arm64|CLI,C API/ADBC,Python,Go,Swift package/framework|refreshed source43 C API passes all155 cases with actual library/signature checks. Historical CP313 client, packaged Swift consumers and typed WAL journeys pass on their stated artifacts. Prior Intel checkpoints remain preserved outside required Mac support. Remaining arm64 client/ABI refresh and final integrated gates remain open|
| iOS device |arm64|Swift package/XCFramework and embedded C API engine|Native arm64 slice builds/content-signs with547 exports, platform2/minimum15.0; packaged Swift consumer compiles/links/signs and selects exact device framework. Device runtime/provisioning remains unverified|
| iOS simulator |arm64; optional x86_64 compile evidence|Swift package/XCFramework, consumer and engine tests|refreshed source43 framework passes all132 upstream Swift tests plus1 identity case on actual iOS26.5 arm64 simulator. Earlier typed consumer and four cross-engine checkpoint/WAL journeys pass. Wider core/C API/ADBC and final integrated refresh remain open. Intel compile/sign evidence is retained without required Intel runtime support|
| Linux |arm64,x86_64|CLI,C API/ADBC,Python,Go,Swift native loading and packages|arm64 signed optimized core/C API155 cases, ADBC64 cases, complete Python fast matrices, Go652 and Swift132+identity plus independent typed client WAL journeys pass. x86_64 source46 now passes C API155, upstream shell392/three skips, Go652 and four independent typed Go WAL/checkpoint journeys; remaining differential/Python/Swift/ADBC, final integrated and native performance gates remain open|

Client combinations follow their upstream desktop/server or Apple-client surface;
this does not introduce new iOS Python/Go clients beyond C++ DuckDB's client support.
Device-slice compilation/signing, simulator execution and physical-device execution
must be reported distinctly. Emulated/Rosetta performance must be labeled with its
actual conditions, and cannot be presented as native-hardware timing.

Every applicable target still needs artifact identities, loading/install checks,
correctness/client/storage/WAL evidence and recorded signing verification. The Mac
arm64 tests do not prove other targets. Full SQL/shell/client and paired performance
acceptance remains tied to the eventual signed artifacts, with individual outliers.
See the [Apple native checkpoint](apple-native-platform-checkpoint.md) for the exact
compiler/artifact scope and simulator storage-access failure.
The [Intel checkpoint](apple-intel-platform-checkpoint.md) records signed slices
and Rosetta-only execution. [Shell evidence](rosetta-shell-parity.md) retains the
shared failures and build-identity differences rather than reporting a full pass.
The [Apple Swift packager](apple-swift-package.md) now requires only three arm64
slices; the older five-slice SDK remains preserved. Mac consumers and typed WAL
journeys pass, and the actual arm64 iOS simulator consumer/four cross-engine
journeys pass using authorized internal storage. Full iOS suites and physical
device execution remain open. Prior [Intel Go evidence](macos-intel-go-client-parity.md)
is historical evidence outside required Mac support.

## Isolated Linux runtime preparation

Docker/Colima/Lima tools exist. The default Docker daemon is unavailable. An unrelated
Colima profile is running; it is not changed or reused for this task. A dedicated
profile is now running under external `platform-matrix-20261029/`, with its physical
VM/config state at `/Volumes/amirs ssd/dk-50198a764d3b1fdc/`. It caps the VM at2vCPUs/8GiB for initial correctness work and does not alter
the host's active Docker context or SSH configuration.

All VM disks, downloaded images, container caches, compiler output, temporary files
and logs must stay under this workspace's dedicated SSD directory. The first `/tmp`
symlink did not solve Lima's length check because Lima resolves it. The task now
indexes the physically shorter SSD profile directory through `runtime/short-profile`;
there is no internal-disk artifact fallback. Runtime keys/disks are not guest-mounted.
Only the dedicated work directory is writable; source/reference/benchmark mounts
are read-only.

The [upstream Colima FAQ](https://github.com/abiosoft/colima/blob/main/docs/FAQ.md)
and [configuration source](https://github.com/abiosoft/colima/blob/main/config/files.go)
document separate config/cache/Docker directories and respect for `LIMA_HOME`.
The driver creates these directories before setting their environment variables,
because a missing explicit config directory can fall back to user defaults. Lima's
separate fixed macOS cache path was absent; a new link directs it to this task's SSD
cache without moving or overwriting existing cache data.

Evidence and the exact start command: `.context/artifacts/platform-matrix-20261029/`.
Do not overlap VM startup or guest builds/tests with another heavy workspace workload.
The Parquet component passed its479-file affected SQL cohort before runtime startup:
473 passes/six exact C++ skips/zero regressions, all fixture hashes stable. Linux
bootstrap now verifies the Rust installer checksum, pins Rust1.99 and the manylinux
image digest, checks969 guest source hashes and runs Cargo portability checks. This
is infrastructure/compiler preparation, not cross-platform release completion.


## Preserved setup failures and runtime proof

Attempt1 failed on overlapping mounts; the reference source was already inside the
read-only workspace mount. Attempt2 completed the SSD image download but failed
Lima's resolved153-character Unix socket path check. Attempt3 uses a physically
short SSD directory (80-character socket path), and starts successfully. The failed
profiles/files/logs are preserved; other projects' profiles are untouched.

The first Docker probe inherited host TLS settings and reached the plain local
socket with HTTPS. Its output is preserved. The corrected task environment removes
only those inherited Docker TLS/context overrides and targets the task socket.
Runtime proof reports Linux/aarch64,2CPUs,7.74GiB guest memory and Docker29.5.2;
Docker and containerd data both reside on the VM data disk, whose backing file is
on the SSD. Workspace/reference/benchmark mounts remain read-only; only `work/` is
writable. `runtime-verification.json` is explicitly an infrastructure gate.

The arm64 builder is pinned as
`quay.io/pypa/manylinux_2_28_aarch64@sha256:a3e02308c822fb6ce0d12248b0c941817ba5e1a1bc9e7bb5929f29c50eaa40a9`.
Its actual digest/architecture and source/bootstrap identities are recorded under
`manylinux-arm64-image-1/` and `linux-arm64-check-1/`. All new toolchains, caches,
targets and logs go to the SSD work directory. Checks have not yet established
release artifacts, Linux correctness or x86_64 execution.


## First Linux arm64 runtime checkpoint

The pinned manylinux arm64 environment installs verified Rust1.99.0 and checks all
969 source inputs inside the guest. `cargo check --locked` passes for C API,shell
and SQL runner. Six SDK test executables then build successfully, are frozen as
ELF64/AArch64 files, and are covered by a detached signed SHA256 manifest.

The Linux OpenSSL verifier checks the signed CodeDirectory/CMS, pinned public
certificate fingerprint, manifest page hashes and every listed file before/after
execution. The SDK binaries run sequentially:42 tests pass, zero failures/ignored,
covering Parquet schema mapping, Arrow producer, main filesystem context, client
settings, persistence and native ART/index storage. These are **debug preflight**
results, not final release or C++/client/OS-matrix completion.

The first optimized build was OOM-killed while rustc linked the shell with full LTO
(actual exit101; Docker reports `OOMKilled=true`). The retry preserves the source
and release profile and adds8GiB of compiler swap on the isolated SSD-backed VM
data disk. Runtime test containers disable swap so this build accommodation does
not change speed/memory acceptance. Both attempts retain separate evidence.

The optimized Linux C API/shell/SQL-runner retry passes with unchanged full LTO.
Four artifacts (shared/static library,CLI,SQL runner) are frozen with source hashes
and a signed manifest. Eight optimized native smoke checks pass with swap disabled:
version,UTF-8/nested CSV values,transaction/checkpoint/reopen,C ABI compile/load/query,
547 exports/no ADBC test hooks,SQL-runner aggregates/rollback,dynamic dependencies
and signed integrity before/after. The initial smoke fixture incorrectly expected
unquoted CSV strings; its failure is preserved. The second attempt parses typed CSV
values; final shell byte-parity remains a separate gate. The full30-file upstream
C API harness passes against this signed library after an explicit `<atomic>` shim
include fixes the GCC appender build. No assertions are removed. See
[Linux arm64 native checkpoint](linux-arm64-native-preflight.md) for artifact hashes,
both retained attempts and the remaining gates.

Toolchains,targets,
registry caches,tmpfiles and logs are under external `work/`; the workspace is
mounted read-only. Source code matches the Mac candidate; the only added source
file is the portable signing verifier, which does not alter compiled engine inputs.
Evidence: `linux-arm64-check-1/`, `linux-arm64-sdk-build-1/`, `linux-sdk-freeze-1.json`,
`work/linux-arm64/sdk-runtime-1/`, `linux-arm64-native-build-{1,2}/`,
`linux-native-freeze-1.json`, `work/linux-arm64/native-runtime-2/` and
`linux-arm64-capi-1/`.

## Detached content-signature verification

Linux payloads are authenticated through a SHA256 manifest signed by the existing
local `rustdb-dev` code-signing identity. The portable helper
`scripts/signing/verify_detached_manifest.py` uses OpenSSL for CMS verification,
validates the SHA256 CodeDirectory page hashes, and hashes each listed payload.
The certificate DER fingerprint is supplied explicitly by the verifier's caller
from a trusted signing record; an untrusted archive's certificate is not a trust
anchor by itself. This is content authentication, not Apple runtime-policy or
notarization verification, and does not export a private key.

The initial direct `security cms` preflight timed out and has no claimed signature
result. `codesign --detached` succeeds and verifies through native codesign and
OpenSSL on the host, then through OpenSSL inside Linux. Manifest,payload and CMS
signature tampering are rejected in preserved controls. A separate initial test
changed unsigned Apple identification metadata; it did not change authenticated
content and was not counted as a signature-forgery rejection.

Primary format references: [Apple's CodeDirectory structure](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/cs_blobs.h)
and [code-signing format/operation documentation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_codesigning/lib/codedirectory.h).
The helper deliberately accepts only a single SHA256 CodeDirectory for a plain
manifest, rejecting ad-hoc,scatter and alternate-directory forms. Final release
artifacts still require their own signed manifest and complete verification.

Current Linux canonical queue1851is prepared and waiting for1847. The existing
8GiBVM/7GiBcontainer C++OOM receipt remains preserved. A separate recorded
correctness-capacity step1835targets the owned2CPUVM at24GiB, retaining its disk,
images, stopped evidence containers and existing build swap. Canonical tests use
equal16GiBengine budgets,20GiBsampled worker RSS ceilings,900second file bounds,
and22GiBcontainer RAM+total limits (runtime swap disabled). These are correctness
conditions matching the current Mac corpus budget, not a change to engine caching
or performance policy. No capacity change or new corpus execution has occurred yet.
Fresh source1634C++oracle test tooling1849is compiled/signed separately from Rust
shipping artifacts; oldoracle source equivalence is not inferred. All4,870original
files and1,522data entries are copied unchanged to fresh native guest storage.
All raw verdicts, resources and individual file times require later review; those
times do not establish TPC-H/ClickBench speed–memory acceptance.
