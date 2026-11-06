# Current release verification — 2026-11-02

Current matrix update: all five declared Mac CP310–314functional cells and
independent reviews now pass (index1831:31,135matching XML records per engine,
392retained nonpasses,36typed WAL journeys,108coupled client samples). Current
iOS ARM build1686is the sole heavy job in1819. Earlier phase updates below are
historical; all other platform/client/storage/focused-control and final native
macro acceptance gates remain open.

Update2026-11-03local: CP313complete and independently reviewed; CP314strict build
is live. Core1637unchanged; no final platform or coupled macro acceptance inferred.

The release is incomplete. Use the current [handoff](handoff.md),
[component verification](current-source-verification.md) and
[required platform matrix](release-platform-matrix.md) for acceptance scope.
Historical checkpoints below remain preserved and must not replace current-image
verification. A build/sign or component pass is not whole-release acceptance.

Common core: source1634, signed macOS arm64 release1637, library SHA-256
`2f34b6d2a2fbeb03bf7a16b873e52f24d33e063b7176e73cf624e5235b9d398d`.
Python leaf: source1742, signed CP310wheel1744 and CP311wheel1752. Equivalence1749
records the five private Python production-file changes; common engine/storage/
Cargo/cache inputs and actual release1637bytes remain unchanged.

Current native evidence index1806covers fresh paired4,870-file SQL collections,
110,464exact CAST queries per engine, shipping HTTP JSON, C API/ADBC, Go/Swift,
workspace, full Mac shell review,20typed storage/WAL journeys and40tail recoveries.
Raw SQL failures/skips, declared unit diagnostics, shell metadata/native frames and
399Mac-unrepresentable filename cases remain explicit; no raw nonpass is relabeled.

Mac CP310/CP311/CP312/CP313/CP314functional reviews1816/1822pass all required installed-client
stages. CP310–313each match6,891raw XML records per engine (6,804pass/87nonpass);
CP314matches3,571records (3,527pass/44nonpass). Their declared420-record boundary
profiles, actual HTTP profiles, six
supplements, typed WAL/encryption,385exports, stubs/typing and installed ADBC.
CP310–313each retain24coupled samples/four rows; CP314retains12samples/two rows. See
[the binding report](python-fetch-conversion-parity.md) for individual slower
Python-backed scans and faster/lower-memory Arrow-backed scans. These client
observations do not establish native TPC-H/ClickBench acceptance.

CP310–313complete their functional stages and independent reviews. Serial
chain1818is building CP314.
Current Apple SDK/simulator, typed iOS and Linux build/sign queues1819/1820/1821
follow serially; native Linux filename collection1825follows the signed builds.
Focused native HF/TLS/S3/secret controls1827follow to refresh historical evidence.
All queued checks remain pending. Physical-device runtime is unverified. Linux
SQL/shell/API/ADBC/selected-client/storage/WAL and final native130configuration
coupled macro acceptance remain required, with per-query outliers preserved.
No cache or query-memory policy change is authorized to close a small speed gap.

Live handles: `.context/corpus-resume-state.json`.
Evidence: `.context/artifacts/platform-matrix-20261029/` on the dedicated SSD.

## Historical checkpoint snapshots

The following earlier text records the state at each checkpoint; later gates
above supersede its "live", "pending" and candidate references.

Current verification checkpoint (2026-11-02): core1637/source1634 remains signed
and unchanged. Both fresh4,870-file corpora and exact110,464CASTqueries perengine
are reviewed:Cpp4839pass/30skip/1knownreferencebug;Rust4826pass/30skip/14documented
rawFAIL. All75nonpasses reviewed, no unexpectedverdict/skipreason changes or resource
kills. ShippingHTTPJSON1713matches completeCppbytes; marker1584resolvedscoped.

Python-only correction1742/signedwheel1744 passes420typed/cursor/retrycases per
Arrowprofile with0differences, bothcompletefastsuites6733records/engine,HTTPprofiles,
all6supplements/eighttypedWALjourneys/encryption,385exports/stubs/ADBC4env/eighttyping
checks andboth79-record non-fastprofiles. Resolution1762closes1718forMacCP310scope;
gate1660positive1763permitsremainingverification. Coreequivalence1749showsall5changed
productionfilesareprivatePythonleaf; no commonengine/cache/querybudget change.

Clientmeasure1758retains24coupledfreshsamples/fourworkloads. Python-backedstrings
are22–24%slower/~1%lesswholeRSS (about8ms);Arrow-backed~5xfaster/~25%lessRSS.
Theseindividualoutliersareexplicit inpython-fetch-conversion-parity.md,notTPC-H/
ClickBenchacceptance. NativeCAPI1661passes30harnesses/155cases/704636assertions;
freshGo1668passes652each/fourindependentWALjourneys1670;Swift1671passes133each.

Onlylivechain1769(exec73721) isShell1678differential/bytecollection. NativeADBC1662
passes64cases3040assertionseach;workspace1765passes149harnesses/1081tests/0fail/7declared
ignores(review1790,2functionalcontrols1785stillpending). Upstreamshell395matches
393pass/2skipboth;remainingbytes/reviews1680–82andstorage1683/84 arepending. Old1764stoppedbeforeADBCworkonastale
receiptpath; recovery1768preservesoriginalsandchangesonlyfourreceiptaddresses.
Allpassedgatesretained. OtherABIs/platforms/SDK/runtime/shellLinux399fixtures and
finalmacro130queryspeed/RSS/outlieracceptanceremainrequired;goalNOTcomplete.

Livehandles:.context/corpus-resume-state.json. Evidence:.context/artifacts/
platform-matrix-20261029/. Fullscope/limits:.context/release-progress-20261102.md.

New selected-client/core gap (2026-11-02): the current CP310macOS wheel1578 builds,
signs and installs into four fresh exact-identity Arrow19/21 environments1564.
Required extension profile1583 now calibrates correctly using a separate bounded
loopback fixture process. C++passes all21records; RustJSON/HTTP/S3 reads pass, then
HTTPException lacks body. Source inspection confirms Python does not copy HTTP
extra_info, core HTTP/status/install errors discard response metadata, and common
extra_info keys cannot yet own arbitrary header_ names. This is verified gap1584,
not either earlier C++fixture failure1570/1582 (both preserved).

Correction source1594 now implements owned metadata keys, raw response reason/header
preservation in a pinned pureRust protocol patch, native HTTP/installer/S3/HF metadata
propagation and Pythonattributes/annotations. Common45units1587 and HTTPFS6units1589
pass;1598 records the one pureborrow lint correction from1588 to1594. Pythonstrict1595
passes, and the optimized developmentCP310wheel1595 is compiling. Runtime C++profile
1599, including complete JSON error bytes, remains required. No Pythoncell is accepted.
Gate1535 now refuses further final ABI/platform builds until a corrected coherent
candidate is verified (negative control1585). Castdiagnostic marker1490 stays resolved
for its separate completed correction. All prior signed native/wheel/component
receipts remain intact; new metadata correction requires appropriate refresh.
See .context/http-metadata-correction-1584.md for exact next code/evidence locations.
Engine cache/query-memory policy and accepted nativecrypto scope are unchanged.

Current signed-candidate frontier (2026-11-02 local): frozen source1523, optimized
macOS arm64 candidate1527. Library SHA-256:
4547c7a66e95e7398ec3bd3f1109ae2c14fa6429bba8c955089f4dbd52319cc0.
Strict1525, build1526, signing/pinned-CMS1527 all pass. The actual optimized library
matches C++ on55,232 cast cases /110,464 queries per engine in1529, including exact
raw errors. Nine behavioral regressions pass, with exact documentation-only lineage1524.

Fresh full SQL1530 completed all4,870 original files:4,826pass/30skip/14fail, zero
crashes/timeouts/resource kills. Review1534 finds zero changed verdicts from1337 and
all30 skip IDs/reasons equal to unchanged authenticatedCpp934 full1302 reference
(4,839pass/30skip/1 independently reproduced catalog-pointer bug). All14 Rust failures
are explicitly retained:11 privateC++engineABI demos and3expected-OOM successful
queries. Twelve failure details are exact; two differ only in independently reviewed
per-run store UUID/directory numbers. The reviewer originally compared reason rather
than failure for FAIL rows; its original script/report are preserved and corrected
failure-detail-review.json retains both raw messages. All source/fixture/signature
checks pass. All4,870 file times and60 filtered individual file outliers are retained;
these are not TPC-H/ClickBench performance acceptance.

Observed normal-profile diagnostic gap1490 is resolved and gate1535 permits further
verification only. Thirty skips stay nonpasses; platform/configuration/extension
profiles require appropriate remaining coverage or explicit exceptions. Legacy1360
is retired with its original preserved1546 so new jobs cannot silently use1337/1344.
Full C API1536 now passes30nativeharnesses/155cases/765,360assertions on exact1527,
including strict signatures and actual loaded image. Paired Go1548 passes652cases per
engine with exactIDs/verdicts. Independent signed consumers1549 and all4typed
checkpoint/nonemptyWAL/mutate/readback journeys1550 pass on current1527/Cpp934.
Swift1551 now passes132upstream+1identity each with exactcaseIDs and actualimages.
StandaloneSwift SDKrelocation/consumer/WAL, iOS and Linux remain separate requirements.
CurrentMac Python compatibility-tool smoke1566 passes. Fresh CP310wheel1578 now
passes actual Maturin/Cargo native compile, signing, wheelRECORD and ABI/ARM checks.
Wheel SHA db58753066c3f3e01892ce5f41fb20dd4fce58c0ccbd19ec8726409205633592;
native SHA22b799ad2571d4f5588ff8ee31fc510a5eb3d5523435ef07e58970a9a5757961.
Fresh paired Arrow19/21 environments1564 pass installation/dependency/nativeidentity
checks; requiredprofile1583 exposes HTTPmetadata gap above, so no Python-cell acceptance.
Initial1539 directCargolink omitted ApplePython dynamiclookup flags; current1578 lets
pinnedMaturin choose exact flags and captures its actualCargoJSON/commands unchanged.
Raw1539 link and1573/1575 precompile wrapper-setup failures remain preserved. Idle
externalAppiumWDA preflight correction1569 is narrow and leaves other work untouched.
Product/core source1523, accepted1527 and all engine memory/cache flags are unchanged.

Further current jobs are prepared for Apple1552-1554,
Linux1556-1557, ADBC1559, encrypted/plainWAL1560-1562, fullworkspace1538, shell1537
and independent1542-1545 reviews. Mac Python1566toolsmoke/1539build/1564install/
1565fullfast are current-source preparations1567; required extension profile1522 and
all supplements/ABI/OS profiles remain required. No success is inferred from preparation.
The dedicated main-disk iOS26.5 simulator is available; read-only probe1547 sees no
physical device, so device runtime remains unproven. Native crypto is allowed, engine
Rust boundary evidence1568 retained. No cache or query-memory policy changed.

Whole release/selectedclient/otherplatform/shell/storage/WAL/coupledperformance gates
remain open. Earlier1337/1344 results below are component history, never final1527
acceptance. The accepted lower-memory tradeoff still requires final native macro
measurements, weighted best/median totals, maximumRSS and every query outlier.

Current frontier (2026-11-01): corrected source1337/signed release1344 has completed
all4870 original Rust SQL files:4826pass/30skip/14documented divergences, zero
crashes/timeouts/resource kills. Reviewer1356 and explicitreview1401/1402 retain
every rawFAIL/SKIP. Thirty skip IDs/reasons match exactC++934 fullbaseline1302
(4839pass/30skip/1independently reproduced catalog-pointer referencefailure).
Reuse lineage1354 is explicit: only2Rustexecutor files changed, all29SLT/oracle/
Cargo inputs identical, exactreference native images remain authenticated.

The only verdict change from1292 is spillcleanup6420 fail->pass. It additionally
passes3original runs,5pairedlifetime originals, fresholdfail/newpass regression,
all16Executionlibtests and CAPI155/755858assertions (audit1357). Raw14remaining
Rustfailures are11privateC++engineABI demos and3expectedOOM successfulqueries;
portableCABI is required, not covered by that exclusion. All2392frozen inputs and
actualsignedlibrary e5e0eb814c43608a5e4ac6647fe90c0066414ac681d84bafed81b2978f8bc4e9
are verified. Every4870file time and111filteredfileoutliers are retained; these
are not TPC-H/ClickBench performance acceptance.

Reviewed client gate1356 permits final verification only. Fresh workspace build1361
and authenticated runtime continuation1410 complete148 harnesses:1,066 tests
passed, zero failed, seven explicitly ignored. All2,392 source inputs, native
signatures and155 signed manifest entries verify afterward. The seven ignores
are five timing/benchmark/comparison tools and two optional full cast-grid/regex
inventory controls; review1412 keeps both functional supplements outstanding.
Original loader setup failures1361/1406 remain preserved; no engine change was
needed. Serial dispatcher1411 has also completed full C API1360:30 files/155 cases/
747,104 assertions on the exact signed1344 library. Both engines complete395
upstream shell cases with equal IDs/dispositions. The768-case differential is
complete:1364 review independently verifies exactly4 build-ID differences and1
known native-frame exception, zero unexpected differences; raw exit1 is retained.
Final Mac shell review1421 covers21,460 original byte cases:20,385 exact
matches,676 native C++ stack-suffix differences and399 unrepresentable filename
fixtures. Exact matches include all2,915 UI and778 stdin cases; prompt has4,780
exact/2 stack differences, numeric rendering2,112 exact/8 stack differences.
Every remaining generator ID, input byte, output/exit, actual image and signed
manifest is checked; source2,392 inputs remain unchanged. The399 Mac fixtures
remain explicit nonpasses and required original Linux executions. Raw suite
nonzero verdicts are retained. No unexpected functional differences remain in
reviewed Mac evidence; all-platform shell acceptance stays open. Native ADBC1434 now passes64 cases/3,040 assertions on each engine, with the
separate3-hook test bridge, successful normal feature-free restore and unchanged
547-export/zero-hook shipping library. Original setup failures are retained;
fixtures and source/native/signature guards verify before/after.

Current Mac storage review1437 verifies20 bidirectional checkpoint/nonempty-WAL
mutation/readback journeys,40 same-input incomplete-tail recovery checks,110
native processes and70 physical headers across plaintext and GCM/CTR×v0/v1.
Exact1344/Cpp934 paths/hashes are retained. Plain wrapper1373 has a stale shipping
label1295; sidecar1437 records actual1344 identity without modifying that receipt.
Mac Go1438 passes652 cases per engine with identical verdicts and exact native
images. Separate signed consumers1367 and all4 typed DB/WAL handoff journeys1425
also pass. Canonical Go runner now lowers parent priority and directly launches
the test binary, preserving DYLD evidence without a failing Python fork callback;
the frozen engine and upstream test assertions are unchanged. Swift1443 passes
132 upstream+1 identity case on each engine. Relocation/standalone Swift consumers,
Swift WAL, iOS and Linux target verification remain separate requirements.

The first CP310 Linux AMD64 wheel is built/signed/authenticated1477 with2,392
source inputs checked. Four fresh paired Arrow19/21 environments1375 pass exact
native-image/dependency/import checks1478. Both complete original fast suites now
match C++ exactly (review1480):Arrow19 has3,241 records per engine, Arrow21 has3,492,
for6,733 matching records each, zero verdict/skip-reason differences. Raw results
per engine are3,205 pass/27 skip/8 xfail/1 xpass forArrow19 and3,455/28/8/1 forArrow21.
Nine supplements1377 are running; the negative controls passed, with nonfast and
remaining package/typing/ADBC/persistence/encryption/producer/memory/measurement
checks still pending. No Python cell is accepted until all supplements and review.

The successful module-only retry1475/guest1474 preserves Rust source and fat-LTO
release flags. Earlier Maturin OOM at7 GiB is retained; recovery1472 adds8 GiB swap
inside the existing SSD-backed VM, preserving physical RAM/CPUs and engine query
memory/cache policy. Initial Clippy1464 and source-matched dependencies are reused.
The packaged wheel SHA is c52faf587e85711eaea594c6d8db551bbda7a4e36dbe1ab9a7c73b020918b1c4;
its native module SHA is b53615b57604df474db6e0760957b9df75abfff5327d54ff76a4c88779a93f87.
These emulated AMD64 functional checks are not native x86 performance evidence.

Filesystem probe1456 confirms invalid-byte filenames round-trip on native Linux
overlayfs but fail on the host-backed artifact mount. The399 original Linux cases
must use native Linux work directories and export JSON/bounded archives to SSD.
Mac build1415/install1429/fullfast1433 are prepared, with all5 ABI dependency profiles
ready including1430 CP313 inputs. Other targets, Apple consumers/iOS runtime, Swift
WAL and130-query coupled speed/RSS acceptance remain open.

No table/rowgroup cache, engine memory-budget or release_on_unpin policy change. Native
thirdpartycryptoallowed, DuckDBengineRust, MacIntel andotherclients outside scope.
Evidence and large outputs remain on the dedicated externalSSD; simulator/runtime
uses the owner-authorized main-disk exception where needed.

Current frontier (2026-11-01): source1337/signed release1344 closes the observed
spill cleanup defect in component audit1357. The unchanged original6420 test
passes three times on Rust; all five original pipeline/hashjoin/window cases pass
both engines. Full public C API passes30files/155cases/755858assertions on the
actual signed library,547public C exports/zero shipping ADBC testhooks.
Freshold-fail/new-pass and all16Executionlib tests are independently verified1352.
Invalid cached after1338 is retained; only fresh-target1346 is corrected-source proof.

All4870 corrected Rust original cases are now rerunning in1355, with identical
fixtures,private extension-store profile and16GiB/20GiB/900s limits. The exact
unchanged C++934 full4870 receipt1302/1330 is retained as oracle:4839pass/30skip/
1known catalog-pointer failure; no old Rust result is reused. Audit1354 records
that only two Rust executor files differ and all29SLT/oracle/Cargo inputs match.
Reviewer1356 explicitly preserves this baseline lineage. No corrected full total
or final client acceptance is claimed until actual completion and review.

Cleanup now releases operator and source data after tasks drain despite remaining
worker metadata references. No row/table cache, memory-budget increase or
release_on_unpin change. Original cleanup timings10.619/10.521/10.610s Rust versus
11.255s C++ are component observations, not macro performance acceptance.

Prior complete1292 full remains4825Rustpass/30skip/15fail, including the now
component-corrected cleanup defect; raw verdicts retained. Final corrected-source
full workspace, shell/ADBC/Python15cells/Go/Swift, required Apple/Linux artifacts/
consumers, DB/WAL and native130query coupled TPC-H/ClickBench speed/RSS/outlier
gates still remain. Earlier1292 final runners must rebase after correctedcore
review. Native third-party crypto allowed; engineRust; noMacIntel.

Current frontier (2026-11-01): complete paired full1302 is reviewed in1341.
Rust4825pass/30skip/15fail; C++4839pass/30skip/1knownreferencefail, all4870 original
files each, no crashes/timeouts/resource kills, all30skip IDs/reasons match.
The six HTTP/cache/file corrections pass. Rust has11 private-C++ABI boundary
failures,3 expected-OOM successful-query divergences and1 real spill-cleanup defect
in leftover_temp_files_issue_6420 after DROP TABLE. No client gate is allowed.

Cleanup correction now passes verified fresh before/after unit controls1352:
old regression fails, corrected regression passes, all16 execution-library tests
pass, strict/pinned signatures and2392 source inputs per control verified.
Invalid cached-after1338 remains preserved; fresh dedicated target1346 is the
valid proof. Corrected release1337/1344 build-sign chain1343 is live, supervised
by1351; then five original paired lifetime cases, two extra unchanged spill
repetitions and CAPI155 run serially. Original SQL/release proof is still pending.
No memory limit/cache-policy increase; release_on_unpin remains unchanged.

Final acceptance remains open for the corrected-source full workspace/SQL,
shell/C API/ADBC,15 Python cells/Go/Swift, required Apple/Linux artifacts and
consumers, DB/WAL journeys and native130-query coupled TPC-H/ClickBench time/RSS
with per-query outliers. Prepared1292 final-client runners are stale until the
corrected common core is accepted. Native crypto allowed; engineRust; noMacIntel.

Current frontier (2026-11-01): HTTP byte/profile integration is now verified on
frozen source1292/signed Mac arm64 release1295 (audit1304). Both engines pass
11 read-profile cases,36 S3 control records including10 upload-profile checks,
and33 unchanged original SQL regressions. Source2392 inputs, selected native
library identity, strict signatures,547 public C exports,zero shipping ADBC test
hooks and pinned manifests are verified. The six earlier I/O functional gaps
remain closed. No full release acceptance is claimed.

Counters match independent receiver body/payload bytes, including GET retries,
PUT/POST attempts and multipart completion; EXPLAIN and ordinary query_tree share
rendering. Per-connection state persists with zeroed counters between queries;
a fresh connection remains isolated. No response/table/row-group cache was added.

Real I/O differences are explicit: JSON issues4 HEADs versus C++1; the small gzip
probe reads160 bytes/two GETs versus C++80/one GET. Small Parquet upload is635 bytes
versus638, with logical readbacks passing. In the14-stream remote gzip probe,
old1283/new1295 times were2.0087/2.0057s direct and3.2331/3.2455s redirect; peak
RSS240.44/240.76MB and240.39/240.63MB. Single samples show effectively unchanged
counter cost. The new Rust image still takes5.79x/5.95x C++ time at0.564x/0.559x
RSS in this workload: explicit remote-input outliers, not TPC-H/ClickBench
performance acceptance. Keep the approved lower-memory policy.

Fresh complete4870-per-engine paired SQL collection1302 is now running serially
on this signed common source, with private extension stores outside the test's
TEMP_DIR whitelist. Do not infer final totals from the targeted controls. The last
completed full receipt remains old source1185/release1188: Rust4819pass/20fail/
31skip, C++4838pass/1fail/31skip, zero crashes/timeouts/resource kills. All original
records and individual timing outliers remain preserved on the external SSD.

Goal remains active/incomplete. Review1302 with prepared reviewer1303, document
all conformance/reference exceptions, and rebase stale1185 final-client scripts
onto the accepted common core. Final shell/C API/ADBC, Python/Go/Swift, required
Apple/Linux artifacts, persistence/WAL and coupled native TPC-H/ClickBench speed/
RSS/per-query-outlier gates remain. Native third-party crypto is allowed; the
DuckDB engine remains Rust. Mac Intel is outside scope.

Current frontier (2026-11-01): signed source1280/release1283 now passes all33
unchanged targeted original tests on both Rust and C++ (zero failures/skips/crashes/
timeouts). Independent audit1286 verifies all2390 source inputs, before/after
fixtures and pinned CMS authentication, four strict native signatures,547 public
C exports and zero shipping ADBC test hooks. The six actual functional failures
identified in full1248 now pass in this component: HTTPS/proxy installer behavior,
remote Parquet one-GET footer/profile counts, attached HTTP logs, failed HEAD
retry timing, remote JSON real external-cache reads and filename-only remote
existence. Previous1273 component31pass/2fail remains preserved.

Memory policy: JSON uses the existing buffer-managed external file cache; remote
footer reads use a bounded16–256KiB prefetch. No new row-group/table cache or
release_on_unpin change. This is not final macrobenchmark performance acceptance.

The last COMPLETE full SQL receipt is still source1185/release1188: all4,870
original files each, Rust4,819pass/20fail/31skip, C++4,838pass/1fail/31skip,
zero crashes/timeouts/resource kills. All31 skip IDs/reasons match; allowed-directory
coverage additionally passes unchanged when its private extension store is outside
TEMP_DIR. Do not combine targeted fixes into an invented new full-corpus total.
All4,870 timings and110 filtered SQL-file timing outliers are preserved separately.

Goal remains active/incomplete. Next: HTTP byte accounting and general profiling
integration, then freeze the final common source for complete SQL/conformance,
shell/C API/ADBC, Python/Go/Swift, required Apple/Linux artifacts, persistence/WAL,
and native coupled TPC-H/ClickBench speed/RSS/per-query-outlier acceptance.
Prepared1185 client scripts are stale for this new core and must be rebased.
Third-party native crypto is explicitly allowed; the engine remains Rust. Mac
Intel is outside scope. Evidence is on the dedicated external SSD.

Current frontier (2026-11-01): complete paired SQL collection1248 covered all
4,870 original files. Rust: 4,819 pass / 20 fail / 31 skip; C++: 4,838 pass /
1 fail / 31 skip; no crashes/timeouts/resource kills. All 31 skip IDs/reasons
match; allowed_directories additionally passes unchanged under corrected private
extension-store placement1265. Raw full results belong to source1185/release1188.

New signed source1270/release1273 passes 31/33 targeted originals; C++ passes33/33.
Four actual gaps now pass: attached HTTP logs, failed HEAD timing logs, remote
JSON external-cache integration and filename-only remote file existence.
The remaining two fail explicitly: installer HTTPS is correct but proxy DNS
wording differs; remote Parquet footer performs2 GETs versus C++1. Typed proxy
resolution and bounded remote footer-prefetch corrections are strict-verified
in source1280, with a fresh release build/sign chain1281 running. No runtime
acceptance yet for1280. Row-group/table caching unchanged. HTTP method counters
are implemented, but HTTP byte counters remain open. Evidence1279 preserves the
31pass/2fail component and every original verdict.

The broader goal remains incomplete: final common-image full SQL, shell,
C API/ADBC, Python/Go/Swift, Apple/Linux packages, persistence/WAL and coupled
TPC-H/ClickBench speed/RSS/outlier acceptance are still required. Third-party
native crypto is explicitly allowed; the engine remains Rust. Required targets
exclude Mac Intel. All evidence/builds are on the external SSD.

Current frontier (2026-11-01): the complete paired SQL collection1248 is finished.
Rust: 4,819 pass / 20 fail / 31 skip; C++: 4,838 pass / 1 fail / 31 skip.
Both covered all 4,870 original files, with zero crashes, timeouts or resource kills.
Inventory1250 preserves every verdict and timing; this is not release acceptance.
The C++ failure is the independently reproduced concurrent catalog metadata defect.
Rust has 11 C++ extension-ABI boundary failures, 3 tests expecting OOM where Rust
succeeds, and 6 unresolved functional gaps: installer HTTPS/proxy behavior, HTTP
profile counts, attached HTTP request logs, failed-request timing logs, remote
JSON external-cache integration and filename-only read_blob remote existence.
Current source corrections are undergoing strict validation1267; no corrected
native release has been built or tested yet. Allowed-directory coverage1265
passes unchanged on both engines when the private extension store is outside
that test's explicitly allowed TEMP_DIR. Full-run skip remains preserved.
Third-party native crypto is allowed; the DuckDB engine remains Rust. Required
clients are Python, Go and Swift. Final selected-client/platform/storage and
coupled speed/RSS/outlier acceptance remain open. Evidence is on the external SSD.

# Release candidate verification, 2026-11-01

The goal remains **active and incomplete**. Source1147 is the common input snapshot
for the optimized macOS arm64 candidate1154. The snapshot has2388 enumerated
inputs and lives on the external SSD. The running corpus and prepared client
recipes use its extracted source, not subsequent worktree documentation edits.

The actual Cargo release build1148 completed successfully. Candidate1154's
feature-free shared C API has547 exports and SHA256
`9c9a6426dd4bfc13d38a6bb3260fa9f6cc3e05c661e424dc220fbffbb611e738`.
Its shell, shared/static libraries, pinned public C header and existing license
notices are in the release bundle. The SQL runner and C++ oracle shim are in a
separate test-tools bundle. Strict native signatures and pinned detached content
manifests were verified before execution.

## Verified on the candidate

| Gate | Result | Evidence under `platform-matrix-20261029/` |
| --- | --- | --- |
| Public C API | 30 files,155 cases,794716 assertions pass; selected image and strict signatures checked | `work/apple/release-capi-1155/verification.json` |
| Upstream shell | Both engines393 pass/2 identical skips; all395 case IDs, statuses and skip reasons match | `release-shell-corpus-queue-1161/shell-verification.json` |
| Shell differential | 768 cases; four reviewed source-ID differences plus one existing native-frame exception; no other differences | `release-shell-metadata-review-1159/verification.json` |

The unmodified differential suite's exit code remains1. Its four metadata cases
were independently replayed (eight observations) against exact expected bytes:
the source-built C++ comparator records dummy ID0123456789, while Rust reports
982c372. Table widths change accordingly. This is an explicit exception, not
literal byte equality for all768 cases. The comparator is native C++1.5.6 built
from pinned source, not an official engine distribution.

The first upstream C++ shell run failed nine cases because upstream tests insert
unquoted temporary paths into dot commands and the SSD path contains spaces.
The existing `shell_ssd_tmp_path` fixture adapter supplies private whitespace-free
workspace aliases to the same SSD directories. Both complete suites pass with
that adapter; no test bodies or assertions changed. The raw failure is retained
in `release-shell-corpus-queue-1160`. The earlier packaging failure1149 (absent
root LICENSE file) is also retained; no tests ran against that partial bundle.

## Running SQL collection

Full paired SQL collection1158 covers all4870 original files, C++ followed by
Rust, with equal16GiB engine budgets,20GiB sampled worker guards and900-second
file bounds. Original fixtures and source hashes are checked before and after
each engine. A private authenticated repository supplies the seven reference
built-ins and official HTTPFS package; HTTPFS is explicitly loaded into private
extension directories. The repository does not supply every optional extension.

An early C++ result for `test/issues/general/test_9384.test` fails because
autoload-mode `require inet` assumes availability but the private repository has
no inet package. The earlier non-autoload corpus skipped that requirement.
Observation1168 preserves this setup issue. Neither the running repository nor
the raw verdict has been changed; its disposition needs separate verification
after both engines finish. Collection completion alone is not acceptance.

Progress is in `release-full-corpus-1158/{cpp,rust}/run.log`; aggregate results
appear when each runner completes. Review helper1169 checks complete unique ID
coverage, raw status/skip-reason differences, source/fixture/authentication
receipts, every nonpass and all file durations. It does not accept exceptions or
substitute corpus timings for macrobenchmark measurements.

## Required remaining acceptance

Finish and review full SQL, then close any genuine gaps. Current-source Mac Go
recipes1163–1165 (652-case suites and four independent typed persistence/WAL
journeys) and Swift recipe1166 (132 upstream cases plus identity) are prepared,
not executed. Preparation1167 pins the current release and independent C++934
images. ADBC, PythonCP310–314 on all three desktop/server targets, broader Go,
Swift packaging and Apple/Linux target refreshes remain open. iOS device builds
and signing must be distinguished from simulator or physical-device execution.

Final artifacts still need cross-engine checkpoint, nonempty/torn/encrypted
database/WAL journeys, coupled native TPC-H/ClickBench measurements with checked
answers, warmup/repeats, time/RSS and individual query outliers, and documented
performance acceptance. Concurrent built-in credential refresh remains unverified.

Preserve `release_on_unpin` and the accepted15–20% time/60–70% lower-memory
tradeoff, together with the conditional corrected COPY exception. No caching,
dependency, COPY memory-policy or engine-code changes were made for this release
verification checkpoint. Third-party native crypto is explicitly allowed; the
DuckDB engine remains Rust. Clients are Python, Go and Swift only; targets are
Mac arm64, iOS arm64 device/simulator and Linux arm64/x86_64. Mac Intel and other
clients remain outside scope.

All evidence is under `.context/artifacts/platform-matrix-20261029/` on the SSD.
The bounded checkpoint is `release-macos-checkpoint-1162.json`. Live process and
resume information is in `.context/corpus-resume-state.json`; verify actual
commands and terminal receipts before starting another serial workload.


## Subsequent remote compressed-input gap

C++ completed all4870 files:4809pass/54skip/7fail, zero crashes/timeouts/guardkills;
independent audit1172 confirms IDs and before/after fixtures/signatures. Six failures
are explicitly observed private-extension setup cases1168; the seventh is the
independently reproduced C++metadata defect1170/969. Thirty-three historical
passes now skip (32 require no_extension_autoloading, one HTTP-error skip), which
must be covered or explicitly disposed of; matching skips alone are insufficient.

Rust's first three files passed. Its original groupby remote-gzip case then spent
over nine minutes in HTTP/CSV input versus C++8529ms. Bounded sample1173 records
active HTTP/TLS reads, not a scheduler deadlock. The exact owned Rust process group
was intentionally terminated for investigation; runner exit-15 and parent exit1
remain preserved. This is an interrupted, incomplete Rust corpus, not a pass or
an observation-tool timeout. The C++complete receipt remains unchanged.

Independent control1174 uses a deterministic6.48MB gzip CSV with160000 rows,
15ms/request latency, direct and redirect paths, exact independent aggregates and
whole-process peak RSS. Rust1154 makes200/400 requests versus C++6/12 and takes
4.063/7.976s versus0.227/0.349s. Both transfer12950734 bytes (two passes).
Rust peak RSS is88.5/88.9MB versus111.5/109.9MB, decimal MB. These are bounded
engineering controls, not the accepted macrobenchmark baseline.

The next source change marks HTTP as a remote filesystem, consistently with S3/HF,
and bounds remote compressed CSV input buffering at1MiB, clipped to file size.
Local64KiB buffering and plain UTF-8 CSV batching remain unchanged. There is no
whole-object/table cache or C++style adaptive growth to32MiB. This change is
UNVERIFIED until a fresh signed build, paired request/time/RSS controls, the real
original test and relevant regressions pass. A new common-source final verification
is required; source1147/1154 receipts are preserved controls, not acceptance of
subsequent source. No performance acceptance or completed Rust corpus is claimed.


## Fixed-buffer results and streaming follow-up

Source1175/signed1178 passes changed-crate strict checks, optimized build/signing,
exact controlled aggregates and the original remote groupby. Control1179 uses the
same6.48MB fixture:Rust requests16/32 (direct/redirect),0.459/0.758s and91.3/90.8MB
whole-process peak RSS. C++requests6/12,0.228/0.351s and111.1/111.3MB.
The original real groupby1180 remains a serious outlier:Rust88.456s versus
Cpp7.770s, sampled RSS1013520KiB versus930096KiB. These results do not accept the
fixed-buffer candidate or justify attributing the whole gap to round-trip delay.

The next correction retains the64KiB CSV input buffer and adds an optional
forward-only reader to registered file handles. HTTP/S3/HF compressed CSV can
consume adaptive1–32MiB HTTP range windows incrementally, without allocating a
window-sized cache. Random-access reads keep exact caller-sized requests. The
same status/Content-Range/ETag validation is shared by both paths. A server that
ignores Range with fallback enabled is consumed sequentially rather than having
its prefix repeatedly downloaded. All public C ABI fields remain unchanged.
This streaming follow-up is UNVERIFIED pending a new signed build and controls.
All previous before/fixed-buffer results remain distinct immutable evidence.


## Streaming component results and require-adapter correction

Source1185/signed1188 passes strict checks, build/signing,12 protocol cases,
14-stream before/after/C++ controls,36 wire cases,31 TLS cases,35 write records,
paired40 secret-transaction cases and30 public C API files/155cases/775956assertions.
The library SHA is a077ff21ef84fdebc18d66d8859b8619015501c84a4dbf267182993fd4776b5a;
independent symbol inspection confirms547 duckdb exports and no private ADBC hooks.
Audit1200 preserves all identities and data; this is component verification only.

Single-stream1189:Rust8/16requests,0.273/0.442s,90.4/90.2MB peak RSS versus
Cpp6/12,0.244/0.368s,113.5/111.3MB. Original remote groupby1190 passes in13.213s
versusCpp7.555s, with sampledRSS818608 versus1090352KiB (not exact peak).
Fourteen-stream1191:Rust2.068/3.359s,239.5/239.6MB versusCpp0.375/0.584s,
430.2/425.8MB. The old slowRust path used198.2/159.0MB and31.424/60.653s;
its memory advantage is not hidden. Remaining time outliers stay explicit, and
none of these controls accepts the final TPC-H/ClickBench macro performance gate.

NONE-profile preflight1197 stopped the controller before fullSQL:both engines
5pass/35skip. The linked-only require helper failed to recognize HTTPFS already
loaded by startup SQL, suppressing31 intended HTTPFS/encryption files. Raw results
are retained. The next test-adapter correction consults actual loaded-extension
metadata only after the linked loader reports unavailable; it never installs a
module or assumes availability. A positive40-file preflight and a negative check
without HTTPFS preload must verify the correction before the full corpus runs.
This adapter change does not alter shipping engine code; new signed tools and
explicit provenance are required. Full goal acceptance remains incomplete.


## Dependency loading must preserve per-file initial state

The full NONE-profile diagnostic1207 accepted3901 C++files before intentional
stop1221:3874pass/24skip/3fail. One is the known C++metadata defect; two original
secret tests explicitly expect HTTPFS to be absent, but the global startup preload
made their CREATE SECRET unexpectedly succeed. This is a confirmed fixture-state
conflict, not a newly attributed Rust defect. Coordinator-accepted worker records,
original diagnostics and intentional-stop evidence are preserved; no verdict is
rewritten or full Rust corpus claimed.

The test adapter now supports an explicit SLT_REQUIRE_EXTENSION_PATHS map. When a
linked loader reports unavailable, it first checks actual loaded state; if still
absent and a trusted path is explicitly configured, it executes LOAD at that
require directive and verifies actual loaded metadata afterward. Global startup
preload is removed. Thus require-httpfs cases get their declared dependency while
unloaded-provider tests remain unloaded. There is no package installation or
assumption that optional modules exist. Shipping engine code is unchanged.
The next preflight must include both formerly conflicting secret files and all40
previous setup/encryption files, plus absent- and bad-path negative controls,
before another full run. All final acceptance gates remain open.


Directive-time load preflight1226 verifies both secret tests and all38 applicable
Rust cases, with4 optional skips. C++30pass/8fail/4skip exposes a further adapter
scope issue:LOAD ran on a temporary connection, leaving the main connection's
crypto utility read-only. Those failures are retained. The adapter now executes
its metadata/LOAD operations on the actual test connection, matching explicit
SQL LOAD semantics. Shipping core remains unchanged; a fresh signed tool and
repeat of all positive/negative controls are required before fullSQL.


The main-connection preflight1236 preserves the same eight C++failures and all38
Rustpasses/four optional skips. Inspection confirms the C++failure is at the first
encrypted ATTACH after restart. The remembered-extension reload path still called
only the linked helper and ignored its unavailable result. The actual remaining
adapter correction replays the same validated external loader after reopening
and reconnecting. Main-connection execution remains appropriate SQL semantics,
but was not sufficient by itself. No shipping-engine change or failure waiver.
