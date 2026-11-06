Current disposition, November 6, 2026: the owner explicitly approved a physical-device test exception. The goal is complete under that scope: all 22 scoped software collection/review requirements are evidenced; actual iPhone/iPad execution and provisioning remain **unverified, excepted by the owner**. Device builds, signatures and simulator tests retain their verified status. The [delivery index](release-candidate-index-20261106.md) links the new signed disposition, unchanged archive and all known limitations. Older dated statuses below are historical.

The exact Mac2556 and Linux2663 engine images have completed the recorded SQL, shell, C API/ADBC, Python, Go, Swift, storage and codec checks. All 15 normal Python wheels are qualified. Final relocation/runtime audit2921 passed, and the signed candidate archive2924 extracted with all 300 payload files and symlink targets unchanged. Known SQL failures, matching skips, reference crashes and shell differences remain explicit; this is scoped verification, not a universal no-exception claim.

The unchanged aggregate performance policy is met: best-time geometric means are 7.2–15.6% longer, with 62.2–78.1% less interpreter-adjusted geometric-mean RSS. ClickBench summed times remain 22–25% longer, and its largest one-thread peak is almost C++'s. The [current performance report](performance-final-native-20261106.md) retains all 63 flagged configurations, four COPY profiles, five whole-file stress outliers and every raw sample. No cache or memory budget was increased.

The live worktree matches 2,439 of the frozen snapshot's 2,446 inputs byte for byte. The seven differences are status Markdown files; no Rust, Cargo, runtime, test or fixture inputs differ, and no additional source files were found. The tested snapshot and raw records remain unchanged.

Historical verification as of November 4, 2026 uses frozen source 2253, signed Mac artifacts 2268 and signed Linux artifacts 2255. The Mac SQL review covers all 4,870 original files: Rust has 4,826 passes, 30 skips and 14 documented raw failures; the unchanged C++ reference has 4,839 passes, 30 skips and one retained reference failure. All 75 nonpasses were reviewed with no unexplained results or skip differences. The 57 individual file-time outliers remain available for coupled measurements. See [current release exceptions and remaining verification](release-known-exceptions.md) for the explicit qualification limits.

Mac focused controls and all CAST/TRY_CAST queries match. The public C API, native ADBC, complete Go and Swift suites, and four typed Go persistence/WAL journeys passed. All 149 workspace harnesses compiled fresh, were signed before execution, and produced 1,085 passes, zero failures and seven unchanged ignores. All five normal Mac Python ABI cells, 3.10 through 3.14, passed independent review: 31,135 matching XML outcomes per engine, comprising 30,743 passes and 392 retained nonpasses. The evidence also covers 36 typed WAL journeys, 216 encryption cases, 18 installed ADBC environments, 40 typing runs and 108 client workload samples. Individual speed/RSS outliers remain explicit; these results do not grant native benchmark or release acceptance.

All three signed Apple ARM64 SDK slices and the full iOS simulator checks have passed their scoped reviews. Both Linux architectures completed the 4,870-file Rust corpus with 4,828 passes, 28 skips and 14 reviewed raw failures each. The shell review then found 531 GNU diagnostic mismatches: C++ emits `Invalid Error: stoull`, while Rust added conversion details. The fix is frozen as source2418; both strict release builds and signature gates passed. The full corrected arm64 shell collection and independent review passed: 21,460 byte cases and all 399 native filename obligations are covered, with documented reference/stack differences retained. All 531 original GNU diagnostic cases now match C++ on both current Linux targets, with exact per-platform stdout, stderr, exit and file comparisons. Both Linux full shell collections and independent reviews have passed, with all original inputs and documented differences retained. The original HTTP logging test also passed on C++ and Rust for both architectures after installing the authenticated offline prerequisite; the original full-suite skips remain unchanged. Both Linux public C API suites passed all 30 harnesses and 155 cases per engine, with independent raw-log and library-loader review. Both Linux ADBC suites also passed all 64 cases and 3,040 assertions per engine, with independent runtime-image, default-restore and public-consumer checks. Go and Swift suites and their independent typed persistence/WAL consumers passed on both Linux targets. A Python-3.10-incompatible evidence hash caused a post-success Swift export failure; completed arm64 controls were recovered byte-for-byte without replay, and x86_64 consumers ran with a streaming hash fix. Independent Go/Swift reviews passed. All five normal Linux arm64 Python 3.10–3.14 cells passed independent review: 30,743 passes and 392 matching nonpasses per engine, plus 36 typed persistence journeys, 216 secure encryption cases, packaging, typing and installed ADBC. All ten Linux normal Python cells (CPython3.10–3.14 on arm64/x86_64) passed independent review:62270 matching XML outcomes per engine (61486 PASS/784 retained nonpasses),72 typed persistence/WAL journeys,432 encryption cases,36 installed public ADBC environments and80 typing runs. All36 client workloads and216 samples remain individual, separate from native macro acceptance. The final HTTP readiness race was corrected with atomic publication and deterministic red/green testing; completed C++ and nine-cell cohorts were reused without replay. Current general cross-engine Linux storage/WAL verification is active. The native Linux build target preserves source, image, compiler flags and memory limits; three earlier native-crypto build failures and the stale-review-receipt failure remain preserved. The actual build/wheel/signature and full client reviews match the selected native bytes; completed cohorts were not replayed during review recovery. General storage/codec and final measurements remain pending. Linux test-observer and fixture setup repairs preserved the existing signed wheel and all data/assertion payloads; completed native stages were reused. The archive export now preserves raw Linux filename bytes that Mac filesystems cannot represent; the complete original ARM tree was verified through two independent reads. Core library/SLT payloads are unchanged, with dependency/module proof2420 preserving their exact evidence scope. Remaining Linux Python, storage/codec, functional controls, COPY and SQL-file measurements, final TPC-H/ClickBench measurements, documented coupled performance acceptance and coherent artifact audit remain required. C++ reference SIGSEGV/invalid-UTF8 errors remain explicit; physical iOS hardware is unavailable and unverified without a waiver. Live paths are recorded in `.context/release-verification-queue-20261104.md` and `.context/corpus-resume-state.json`.

Current corrected Mac evidence (2026-11-04): source2253/signed2268 full4870 collection and independent review2279 complete. Rust4826PASS/30SKIP/14documented rawFAIL; unchanged C++ reference4839PASS/30SKIP/1retained referenceFAIL. All75nonpasses reviewed,0unknown/0skipdiff;57whole-file time outliers retained for current coupled measurements. Focused2276 allsixcohorts1893literalrecords each match; allCAST2277 55232cases/110464CAST+TRYqueries perengine match. Both original checkpoint-reader and index-publication races pass inside the current Mac corpus. CAPI2280 is active. These are scoped Mac component results; full selected-client/platform/storage/shell/runtime/performance and physical-device acceptance remain incomplete. See `.context/release-verification-queue-20261104.md` for the serial continuation.

Current corrected-artifact execution (2026-11-04): recovery build2267 and signing2268 completed successfully on frozen source2253. The Mac library SHA256 is 11a1047d91ba57efd71c854b11e530914b494780ad760e9b4f90db5af3c32cfb. Fresh Rust full4870 collection2274 is running after signature authentication under controller2275; focused2276 and all-cast2277 follow serially under2278. Both signed Linux2255 bundles remain ready for their fresh complete corpus runs. No full-corpus, selected-client, persistence/WAL, codec or performance acceptance is claimed by the build/signing receipts.

Current build recovery (2026-11-04): both corrected Linux core bundles 2255 are built and signed, independently indexed in 2265. A host restart interrupted Mac build2260/controller2262 before a successful release terminal receipt; evidence2266 preserves that interruption. Recovery2267 uses the same frozen source2253 and a fresh release rebuild of the three target packages; strict all-target checks pass. Signing2268 precedes every execution. Controller2275 waits for signing before collecting all4870 original Rust cases in macos-full-corpus-2274, with the unchanged completed C++2091 raw reference copied byte-for-byte and its original failure retained. Linux collector2272/controller2273 and focused2276/all-cast2277 are prepared but unexecuted. These preparations grant no corpus, client, storage or performance acceptance. All23 completion requirements remain in scope.

Corrected native checkpoint verification (2026-11-04): frozen source 2253 / signed Linux x86_64 artifacts 2255 pass all 30 Rust and three C++ runs of the unchanged concurrent checkpoint/delete test. Independent review 2263 verifies every raw worker, original source, fixture/signature stability and zero OOM/max events. Together with the deterministic signed red/green and 125 strict component tests, this resolves the observed checkpoint-reader failure for the original case. The existing source-2078 artifacts stay retired; they are not made valid by this correction. Source gate 2264 permits broader corrected verification without granting release acceptance. ARM build 2259 and Mac build/signing 2260/2261 run serially next; all full corpus, shell, C API/ADBC, Python/Go/Swift, storage/WAL/codec, coupled performance and physical-iOS gates remain required.

New correctness fix in progress (2026-11-04): source 2078 / signed Mac 2088 and Linux 2080 are retired from final acceptance. The unchanged concurrent checkpoint/delete test failed on Rust with an invalidated database: destructive metadata conversion encountered active readers. The current C++ control passes and its metadata flush explicitly uses THREAD_SAFE conversion. Rust now copies that metadata block before persistence, keeping old readers alive. Signed deterministic verification fails on the old path and passes after the fix; all nine buffer/storage harnesses pass 125 tests with no ignores, and strict all-target checks pass. Existing lower-memory cache policy is unchanged; the temporary metadata copy is required for correctness and matches C++. The original native SQL case still needs corrected-artifact replay, followed by a new coherent candidate and all final gates. All prior receipts retain their source/artifact scope; none accepts the corrected code. Evidence: checkpoint-reader-regression-2251, checkpoint-reader-component-verification-2252 and concurrent-checkpoint-reader-gap-2250 under the external artifact directory. The goal remains incomplete.

Current recovery (2026-11-04): an external VM/host-controller interruption stopped the prior Rust run at 2,825 of 4,870 cases. Evidence 2237 preserves that partial collection and the stopped container (exit 255, Docker OOM flag false). The existing owned VM is restored with the same CPU, RAM, disk and image IDs. Its previous build-swap file is reactivated; the SQL container still has no swap. Fresh collection 2238 reruns all 4,870 Rust cases from canonical native fixtures under host controller 2239. The completed C++ collection is retained byte-for-byte with explicit provenance: 4,841 passes, 28 original skips and one unapproved SIGSEGV. Fresh-run review 2244 checks the first 12 actual Rust nonpasses; older partial-run dispositions do not accept the new run. Serial root 2241 and independent review tail 2246 restore every remaining gate, including the codec refresh. Current counts are in `.context/corpus-resume-state.json`. No engine memory/cache policy changed, and final release/performance/physical-iOS acceptance remains incomplete.

Current x86_64 checkpoint (2026-11-03): the recovered C++ collection 2215 is complete at 4,870 files: 4,841 passes, 28 original prerequisite skips and one unresolved SIGSEGV worker crash. Independent review 2233 verifies all records, skip clauses, native/source/fixture signatures and zero OOM/max/resource-kill events. The crash in `float_decimal_cast.test` remains raw and unapproved; its core is preserved in evidence 2226. The fresh Rust collection is now running. The original 40-GB, extended-precision and concurrent-index C++ cases all pass. Scoped cast controls 2227 and all remaining client/shell/storage/performance/artifact/hardware gates remain required. A final-artifact codec coverage audit also added the original FSST/DICT_FSST WAL and ZSTD Parquet integrations on all three native targets; signed preparation 2231/2232 is queued, not passed. The goal is incomplete.

Recovery in progress (2026-11-03): the full x86_64 pair is being rerun in collection 2215 with the same signed source 2078 / shipping 2080 binaries, original tests and engine memory limits. The only runtime adjustment is a higher inherited file-descriptor limit. Diagnostic 2214 passed the first unrecorded case on both engines; observation of growing `/proc/<pid>/auxv` descriptors in the translated monitor supports descriptor exhaustion as the cause of the earlier Rosetta stop. The original failed collection and evidence 2213 remain preserved. Serial dispatcher 2222 will resume the unstarted components after the recovered corpus succeeds. Independent AMD nonpass review, final coupled performance acceptance and physical iOS verification remain required; the goal is incomplete.

Latest environment stop (2026-11-03): Linux x86_64 reference full2131 stopped at828/4870 (821pass7skip0SQLfail recorded) with Rosetta temporary-file assertion/actualexit133; no OOM kill and no verdict for the next case. All downstream queues stopped on this prerequisite; their payloads did not run. Evidence2213 preserves the stopped container, raw logs and native fixtures. Native disk has63GBfree and TMPDIRexists. Isolated unchanged nextcase2214 Cpppasses; diagnosis/recovery remains in progress. Current ARM fullSQL2212 and completed Mac/Apple/client receipts remain valid. Final performance and physicaliOS acceptance remain incomplete.

Current verified macOS SQL/C API checkpoint (2026-11-03): both fresh4,870-file collections and independent review2093 are complete on corrected source2078/signed2088. Rust has4,826passes,30shared skips and14documented divergences (11private C++engine ABI cases;3tests expecting OOM where Rust succeeds). C++has4,839passes,30skips and one diagnosed metadata-concurrency reference failure. All75raw nonpasses are retained/reviewed; there are zero unknown nonpasses, skip differences or resource kills. All55individual SQL-file timing outliers are retained; these file timings are not TPC-H/ClickBench performance acceptance. The original concurrent-index case passes on both engines. C API2094passes all30harnesses/155cases/705,024actual assertions; independent2187checks all raw summaries/native/source pins. Assertion totals differ only in the unchanged pending/streaming polling loops. ADBC2095nowpasses64cases/3,040assertions per engine; independent2190verifies all original case names, selected libraries and separate-testbridge/default-restore/signature evidence. Shipping remains547exports/zero private hooks and the same library hash. Workspace2096nowpasses149harnesses/1,083tests with zero failures and seven declared ignores; independent2191verifies every original harness/raw summary/native hash and the exact two added regressions. Required ignored CAST/RE2 supplements remain pending. Matrix2104nowpasses all five fresh CP310–314normalABI cells with independent reviews:31,135matching XMLrecords per engine/30,743passes392retainednonpasses,36typedWAL journeys216encryptedParquet cases18installedADBC environments40typing runs18individualclientworkloads/108samples. CurrentGo2107passes652cases per engine and independent2109fourtypedWAL journeys; Swift2110passes all133upstream/identitycases perengine. Independent2198checks currentGo/Swift/native/typedhandoffs. CorrectedApple2128/independent2126nowpasses all3signedARM SDKs with547exports each,MacSDKrelocation/2normaltypedconsumers/4WAL journeys,all133actualsimulatorSwiftcases,typedconsumerand4additionalcrossengineWAL journeys. Main-disk simulator use is authorized. Physicaldevice runtime/provisioning remains unavailable/unverified/notwaived. CorrectedLinuxx86_64build2082/sign2080nowpasses, alongside ARM2079/2080:both2451inputsource2078 builds have547exports/zeroADBCtesthooks andcurrentcontent signatures. Read-onlycapacity2130confirms64,146,907,136nativefree bytes with unchangedCPU2/RAM24GiB/disk100GiB. FreshLinuxARM Cppfull2131nowcompletes4,870files:4,842passes28originalprerequisiteskips0failures. Independent2206checks all28worker/sourceclauses/nativefixtures/CMS/soleCppimage andzeroOOM/max/resource-kill events;40GBcaseand2Linuxextendedprecisioncasesexecute/pass. ARM Rustfull2131nowcompletes4,870files:4,828passes28skips14documentedrawFAIL. Independent2212reviewsall9,740ARMrecords/70rawnonpasseswith0unknown/skipdifferences/OOM/max/resourcekill events; all28literalprerequisite/source/worker checks andall41individualfiletimingoutliers retained. Original40GB/concurrent-index20001/Linuxlongdouble cases pass onbothcurrentengines. x86_64 Cppfull2131is now active, followedbyRust; theseRosettafunctionalresultsarenotnativex86performance. Equal16GiBengine/20GiBsampledRSS/900s/jobs1/22GiBnoswapbudgets remainunchanged. AMDfullrecords/nonpassreviews andremainingLinuxclients/APIs/shell/storage/Macstorage/COPY/outliers/native130macro/finalartifact/hardware gates remainrequired. Other clients/platforms/shell/storage/WAL/coupled-performance/physical-iOS gates remain required. The goal is incomplete.

Corrected candidate frontier (2026-11-03): source2078 contains the index-publication
fix: the existing DataTable append lock now spans the build scan through index
registration. It adds no cache, buffer or query-memory budget. Controlled2077fails
on old code (indexrowIDs{0}, table{0,1}) and passes after the fix; all10constraint/
index tests and strict storagealltarget checks pass. Linux ARM build2079/sign2080
has547Cexports and zero private ADBC hooks. Its shared-library SHA-256 is
`e53d7d6c32788b472e2bd27dd072201c7db5ffbbc9e50d47496b96efdccab6c1`.

Independent2085checks the unchanged whole original concurrent SQLcase: oldsigned
Rust fails14/30runs; corrected2080passes all30, with3Cppcontrols passing. This
resolves the observed nativeARM race; whole-candidate correctness is not accepted.
Source1959/core1961/Linux2005stay permanently retired from final acceptance; their
original failures/partialcollections/signedpayloads are preserved.

At a verified idle checkpoint, disk-capacity2083changed only the ownedLinuxVMdata
disk60→100GiB. CPU2/RAM24GiB/context/images/stoppedcontainers/existingbuildswap are
preserved; actualnativefree space is64,148,267,008bytes. The original40gb prerequisite
SQLcase now executes and passes on bothCpp and correctedRustARM2084; originalraw
skips remain. CorrectedAMD/originalfullcollections still need verification.

Mac strict/release2087 completes actual0; sign2088 authenticates547Cexports/zero
private ADBC hooks. Current librarySHA:
`2a34c28766702536cbe4a87a5a48a33410b2e0d333d6e3c61e6783da0d7784df`.
Serial nativeverification2092/session68951 now runs full4,870-file pairedMac2091.
Both engines freshlypassed allsixfocused cohorts2089 (1,893records each,0diff)
and all110,464CAST/TRYqueries each2090 (0diff, ICUloaded/UTC). Every fullcorpus
rawnonpass/resource/fileoutlier still requires currentindependentreview. All applicable core/SQL/shell/CAPIADBC/PythonGoSwift/AppleSDKsimulator/
Linux/storageWAL and130nativecoupledspeedRSS/documentedacceptance gates must use
the corrected coherent artifacts. PhysicaliOS is unavailable, unverified and not
waived. The goal is incomplete. Live pointers: `.context/corpus-resume-state.json`.

Earlier frontier text below is historical and retains its original evidence scope.

Open correctness gap (2026-11-03): current source1959 is not release-ready.
Linux Rust ARM's original concurrent_writes_during_index_creation.test_slow
returned indexedCOUNT1,682 instead of20,001 after its grouped table count passed.
The current C++reference passes. Exact unchanged native replay2076 reproduces the
mismatch in14/30Rust runs; all3Cppcontrols pass. This is a new correctness failure,
separate from the documented privateCppABI/expectedOOM cases. Evidence2075keeps
original source/worker/partiallogs and the controlled stop of only the owned ARM
corpus container. Dispatcher2069/session59928 is terminal1; later roles are unstarted.
No full Rust/Linux corpus acceptance is inferred from that incomplete collection.

The controlled storage regression2077 fails on old code with indexrowIDs{0}
versus table{0,1}; after holding the existing append lock across build/publication,
the signed nativeMac harness passes all10constraint/index tests. Strictall-target
storageClippy also passes. No cache/querymemory/buffer policy changed. The corrected
source2078 still needs fresh nativeLinux original-case replay and a new coherent
signed candidate followed by all required final gates. Source1959/1961/2005 remain
retired from final acceptance;2054 rejects their known correctness failure.
Prior component receipts below retain their original native bytes and scope.
The disk-prerequisite gap2071 and unavailable physicaliOS hardware also stay open.

Current frontier (2026-11-03): source1959/core1961 is the signed macOS arm64
candidate after the S3 upload-context fix. Its library SHA-256 is
`e65b5ac5f6e05912a9bd6d81d095445f0b701b82d1c3bacfb0f63c359feafe31`.
Strict checks, nine HTTPFS unit tests, six native control groups (1,893 records
per engine), the permanent S3 regression and 110,464 exact CAST/TRY_CAST queries
per engine pass.

Both complete 4,870-file macOS SQL collections are reviewed in2022: C++4,838pass,
30skip and two raw failures; Rust4,826pass,30skip and14raw failures. All76nonpasses
are classified and retained; there are no unknown verdicts, skip differences or
resource kills. Exceptions remain explicit in `conformance/divergences.md`.
C API1971 passes30harnesses/155cases/745,460assertions. ADBC1972 passes64cases/
3,040assertions per engine. Workspace1973 passes149harnesses/1,082tests with seven
declared ignores; independent inventory2058 confirms the sole additional pass is
the new HTTPFS regression. Shipping1961 retains547C API exports and no private
ADBC test hooks.

All five current macOS Python3.10–3.14 functional cells and independent1980
reviews pass; matrix2024 exits0. Summary2062 verifies31,135matching XML records
per engine,30,743passes and392retained nonpasses,36typed WAL journeys,216encrypted
Parquet cases,18installed ADBC environments,40typing runs and108coupled client
samples across18separate workloads. These client measurements do not establish
macro acceptance; individual rows and warnings remain in
`docs/python-fetch-conversion-parity.md`.

Current Go1982 passes652cases per engine; independent1984 passes four typed
checkpoint/WAL journeys. Full Swift1985 passes133cases per engine. Dispatcher2032
stopped before iOS compilation because the source verifier assumed workspace CWD.
Recovery2063 preserves that failure and verifies the same source/native/certificate
checks from both working directories. New dispatcher2065/session65225 preserved the completed Mac roles. Apple2064 now
exits0; independent2001 verifies all three signed ARM SDK slices with547Cexports,
Mac SDK relocation/two normal consumers/four typed WAL journeys, all133actual
simulator Swift cases, the typed simulator consumer and four additional WAL
journeys. Exact133case IDs agree on C++Mac/RustMac/RustiOS. Simulator runtime uses
the authorized main disk. Physical device execution/provisioning remains unverified.
Both Linux ARM2003/x86_64 2004 builds and signed freezes2005 pass with547Cexports/
zero test hooks each. Dispatcher2065 then stopped on a stale verifier pin in the
corpus script_hashes preparation before any corpus payload. Recovery2067 preserves
that failure and checks both preparation map formats; no engine/test/native or
crypto pins changed. Dispatcher2069/session59928 resumes at corpus2068. ARM C++
collection2010 now completes4,870files:4,841passes/29skips/0failures, actualterminal0.
Reference review2073checks every skip/source prerequisite/worker/nativefixture and
zeroOOM/resourcekill events;28skips match MacCpp, one40gbcoveragegap stays open.
TwoLinuxlongdoublecases execute/pass and the twoMacCppconcurrencyfailures do not
reproduce. Rust ARM collection is now running; both AMD collections and final
Linux-specific dispositions remain required.
Correctness capacity2008 changes only the owned VM8→24GiB, retaining CPU2/disk60/
images/stopped containers/build swap. Both engines use16GiB budgets with20GiB worker
RSS guards,900s/file,one worker and22GiB/no-swap containers. This is correctness
capacity, not performance or a cache-policy change.

Early Linux ARM C++ review2070 identifies one unresolved coverage gap2071:
`distinct_grouping_tpch.test_slow` requires40gbfree native disk; the guest has
about24.2GBfree and skipped it, while current Mac C++passed. This is the only
disk-space prerequisite in the4,870original files. Preserve the rawskip and
execute the unchanged full original test for both engines/architectures with its
prerequisite met before finalSQLacceptance. Current collection remains running;
no prerequisite, corpus filter, native test or memory/cache policy was relaxed. PhaseB2047 (Linux Python, storage/
WAL and full macOS shell) and the130configuration native benchmark2055 are prepared,
not passing evidence. Linux SQL nonpasses still require their own disposition
review after collection. Final artifact audit and documented coupled speed/RSS
acceptance remain required. Preserve `release_on_unpin` and the accepted lower-
memory tradeoff. Physical iOS hardware is unavailable, unverified and not waived.

Old source1634/core1637 and final gate1660 are historical, retired from final
acceptance. Gate1965 permits further verification only. The goal is incomplete.
Live handles: `.context/corpus-resume-state.json`. Evidence:
`.context/artifacts/platform-matrix-20261029/` on the external SSD.
Earlier checkpoint text below is historical; its running/pending descriptions do
not replace this frontier.

Current Linux ARM byte review1879passes all10,865original argv/path records:
all399Mac-blocked filenames execute/match;666raw mismatches are validated native
C++stack suffixes with exactpreamble/status/otherstream/fileeffects,0unexplained.
Rawverdicts are preserved; opaqueDBcontent is outside the originalshell contract.
x86_64byte collection is live. Other Linux shell/core/API/clients/storage/WAL and
final130native coupled macro acceptance remain required; goal incomplete.

Current Apple review1848(2026-11-03local) passes the signed three-ARM-slice SDK
(547Cexports each), Mac SDK relocation/standalone consumers and four typed WAL
journeys, full133case iOSsimulator Swift suite and typed consumer/four additional
cross-engine persistence journeys. The133raw case IDs match C++Mac/RustMac/RustIOS.
All signatures, native images and typed values verify; no physical-device execution
or provisioning is proven. Simulator payloads run on the authorized main disk.

Linux ARM1838is now building offline under1845exec36384. Missing locked rsa cache
caused1821to stop before compilation. ARMfetch1837succeeded; a null-source observer
error stopped1841. Recovery1844retains the successful fetch and verifies153locked
registry archive checksums per architecture plus offline metadata. No versions,
Cargo.lock, engine code or memory/cache policy changed. Native filename1846and
focused-control1847queues follow. Prepared VM correctness-capacity1835is unqueued.
AllLinuxruntime/APIADBC/client/storage/fullSQL and native130query TPC-H/ClickBench
coupled acceptance remain open. The goal is incomplete.

Latest verified update (2026-11-03 local): all five declared macOS ARM64 Python
ABI cells (CPython3.10–3.14) pass functional stages and independent reviews1816/
1822/1831. Across profiles:31,135matching XML records per engine,30,743passes and
392matching nonpasses retained.36typed WAL journeys,216encryption cases,18installed
ADBC environments,40typing runs and108coupled client samples verify. Every client
workload remains separate; CP314Python-backed timing is about29%longer/~0.8%lessRSS,
and its Arrow-backed row is~0.218time/~0.816RSS. Reference-only Pandas/pytest
diagnostics remain explicit. These are not final TPC-H/ClickBench measurements.

Current iOS simulator compilation1686now passes actualcompiler0/buildfinished,
freshopt3 and feature-free shipping CAPI checks; device ARM slice is compiling.
This is compilation evidence, not signed SDK or simulator execution acceptance.
Linux arm64 HTTPFS reference1833passes official-core RSA and tamper-negative checks
plus pinned local content CMS; no module has executed. Exact package identity and
the preserved initial key-count observer failure are in the platform matrix.

Mac Python batch1818actually exits0. Apple1819exec92064is now building current
iOS ARM slices1686, the sole heavy workload. SDK/relocated consumers, full simulator,
typed iOS, physical-device, Linux runtime/clients/storage and focused native1827
gates remain open. Fresh hardware query1829sees12simulated/zero physical devices.
Main-disk simulator runtime is authorized; evidence remains on the SSD. Goal
incomplete; live process references are in .context/corpus-resume-state.json.

Earlier component checkpoints follow; their phase-specific "live"/"pending" text
is historical and does not replace the current state above.

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

IndependentCP310review1816 verifies all12receipts/signatures/nativehashes and
6,891rawXMLrecords perengine:6,804pass/87matchingnonpasses,0differences in complete
outcome/message/body. It recomputes all24client measurement samples; scoped functional
review complete, final release acceptance still open.

Clientmeasure1758retains24coupledfreshsamples/fourworkloads. Python-backedstrings
are22–24%slower/~1%lesswholeRSS (about8ms);Arrow-backed~5xfaster/~25%lessRSS.
Theseindividualoutliersareexplicit inpython-fetch-conversion-parity.md,notTPC-H/
ClickBenchacceptance. NativeCAPI1661passes30harnesses/155cases/704636assertions;
freshGo1668passes652each/fourindependentWALjourneys1670;Swift1671passes133each.

Current native macOS verification is consolidated in SSD receipt1806. ADBC1662
passes64cases/3,040assertions per engine; workspace1765 passes149harnesses,
1,081tests and seven declared ignores. The two functional supplements now executed:
cast1785 has52,953implemented cases and54,171flat-vector rows with zero mismatches;
RE2 library control1802 passes the complete corpus pattern inventory. Raw unit-only
ICU diagnostics retain32mismatches/45blocked cases; actual signed SQL1639 matches
all110,464queries per engine, including ICU. Five diagnostic tools remain ignored.

Full Mac shell review1796 covers395upstream cases (393pass/two matching skips),
768differential cases with five documented metadata/native-frame exceptions, and
all21,460byte cases:20,385exact,676native-stack suffix differences and399filename
fixtures unrepresentable on macOS. The399original cases still require a native
Linux filesystem. No raw failure is converted to a pass.

Encrypted storage1683 and plain storage1684 now pass20bidirectional typed
checkpoint/WAL journeys and40appended/truncated-tail recoveries across110native
processes. Both exact signed images and independent typed values are verified.

CPython3.11 now completes build1752, installation1753, HTTP1754, boundary1770, both
fast1755profiles, all six supplements, package/typing/ADBC, non-fast and24coupled
client samples. Independent review1822 passes6,891raw XML records per engine
(6,804pass/87matching skips+xfails), all exact identities and15stage terminals.
Python-backed client rows take about21%longer with0.6–1.3%lessRSS; Arrow-backed rows
take about one fifth of the time with24%lessRSS. Rows remain separate; no macro
acceptance inferred. CP312 now completes all15stages and independent1822review with6,891matching XML
records per engine. All24client samples remain separate:Python-backed1.250/1.214time
and0.997/0.991wholeRSS;Arrow-backed~0.190time/~0.807RSS. CP313signed wheel is built; installed-client verification is live
in1818;CP314follows. Focused native controls1827are queued after Linux filenames;
older TLS/secret controls are not substituted for final-image verification.
Original1804provenance preflight failure and recovery1817 remain preserved.

Apple1819 waits for1818, then refreshes the three ARM slices, Mac SDK consumers/WAL
and full simulator suite. Typed consumer chain1820 follows, then Linux build/sign
chain1821. Original dependent queues1805/1810/1814 stopped before launching work.
Queued checks are not passing evidence. Physical iOS, Linux runtime and final130
TPC-H/ClickBench speed/RSS/outlier acceptance remain open. The goal is incomplete.

Livehandles:.context/corpus-resume-state.json. Evidence:.context/artifacts/
platform-matrix-20261029/. Fullscope/limits:.context/release-progress-20261102.md.

Historical checkpoints below (superseded by the current frontier above).

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

# Current-source verification

**Current frontier, 2026-10-31:** the goal remains incomplete. Required targets are
macOS arm64, iOS arm64 devices/simulators, and Linux arm64/x86_64. Mac Intel is
outside the required scope. Clients are Python, Go and Swift, preserving C API/ADBC.



HTTPFS now uses a vendored OpenSSL connector (review824) with pinned CA-file search
and Apple-native missing-anchor supplementation. Strict httplib explicit-CA mode
does not supplement that file. Native-tls experiment780 did not match C++ and was
not adopted; Rustls/native-tls engines are absent from final dependencies. No
verification bypass, object/table/query cache or memory-budget increase.

Signed815/SDK816 freshly built814 match C++ in31 TLS/HF cases/293 literal records:
CA-marked endpoint accepted, wrong-host/expired/untrusted chains rejected, flags
and CDN auth behavior retained. The former CA-marked Mac acceptance discrepancy
is closed in these controls. Broader TLS/system-root and Linux/iOS coverage stays
open. Public metadata probe829 matches12 records: default curl/strict httplib
trusted roots accepted; strict httplib unrelated explicit CA rejected. Earlier806
caught a real ETag change after TLS; metadata probe keeps ETag verification on.

The same image preserves30 partition SQL+11 SDK818,35 upload records819,30 S3 read/
list cases/276 records820,16 HTTP cases/121 records821,59 extension cases/246
records822 and full public C API823:30 files/155 cases/792002 assertions. Strict
workspace/all-target Clippy813 passes. Shared native SHA256:
81768d3d5387b8f7f00ded04c701d926140f1ce6ebeeb2a2a19287c4c30101c3.
All native signing/CMS checks before/after pass. Allocation sample remains16.5MB
EXTENSION on both engines, zero before/after; this is not RSS/performance acceptance.

Remaining: broader TLS/root/malformed-CA/proxy/platform controls, HTTPFS options,
region redirects/refresh/version pinning, directory/configuration/autoload/startup,
secrets transaction/storage gaps, default HF token branch/real services, remaining
core gaps and all final common-source client/platform/SQL/shell/CAPI/ADBC/DB/WAL/
coupled speed/RSS acceptance gates. See `docs/httpfs-openssl-tls-parity.md`. Goal
active/incomplete; required scope and accepted low-memory policy unchanged.

Earlier HF checkpoint follows; the OpenSSL checkpoint above is current.

Native HuggingFace files/tree traversal and secrets are implemented (review767).
Signed image758 matches C++ in23 cases/223 literal SQL records: datasets/spaces,
revisions, wildcard recursion/pagination, scoped/config/credential-chain tokens,
HTTP header precedence, invalid paths/responses, missing files and same-host/CDN
redirects. CDN authorization is stripped on both engines. Controls use a synthetic
CONNECT receiver that never opens an upstream socket; no default user token read.
No object/table/query cache, memory budget or release_on_unpin policy changed.

Known TLS distinction remains: C++ accepts CA-marked fixture735, while Rustls
rejects its use as server leaf744. Proper leaf751 passes on both engines; no
verification was disabled. Default-token-file and full TLS/system-root coverage
remain open. Original nested-scope accessor crash736, style738, absent-type before
739 and certificate744 failures are preserved. Prototype753 matched22/213 before
the empty-HF_HOME control; final23/223 includes that edge.

The same library retains30 partition SQL+11 SDK records761,35 upload records762,
30 S3 read/list cases/276 records763,16 HTTP cases/121 records764,59 extension cases/
246 records765 and full public C API766:30 files/155 cases/773982 assertions.
Strict workspace/all-target Clippy756 passes. Fresh shared758+SDK759 build757,
strict signatures and whole-file pinned CMS verified before/after. Shared SHA256:
13c268dc921438a97252f5fc120a4195bfc0bc77cfbd785a7c16774dd4071fcf.

Remaining: isolated default HF token path, real service and full URL/JSON/protocol/
TLS/system-root coverage, secrets transaction/storage gaps, HTTPFS options, region
redirects/refresh/version pinning, directory/configuration/autoload/startup edges,
remaining core gaps and all final common-source client/platform/SQL/shell/CAPI/
ADBC/DB/WAL/coupled speed/RSS acceptance gates. Required scope unchanged; extra S3
prepared listing still explicit. See `docs/httpfs-huggingface-parity.md`. This is
component evidence, not full HTTPFS or release completion.

Earlier partition/delete checkpoint follows; the HF checkpoint above is current.

S3 partitioned COPY and single/bulk deletion are implemented (review730).
Signed production image714 plus corrected SDK726 match C++ in30 partition SQL
records and11 SDK removal records. Overwrite modes, append, CSV/Parquet, encoded/
NULL partitions, object/prefix coexistence, 1005-key deletion and scoped-secret
batch separation are covered. Missing/denied/disabled cases match literally.
Before701 preserves image685's real unsupported-directory failure. No memory
budget/table/query cache or release_on_unpin policy changed.

Production image714 was freshly built713; only the SDK probe output allocation
changed afterward. SDK726 rebuild725 passes; the root library remained cached and
byte-identical to713's original artifact (continuation727 records this). C++ SDK710
and reference729 are independently authenticated. Original fixture699, probe705/708,
style702 and SDK716 failures are retained, not hidden.

The same image retains35 functional upload records717,30 S3 read/list cases /276
records718,16 HTTP cases /121 records719,59 extension cases /246 records720 and full
public C API721:30 files /155 cases /775682 assertions. Strict workspace/all-target
Clippy728 passes. Native SHA256:
191fbfb115d9f0f2b59ab9f6ad681dc12424d692205a8bc22df8be2d93aba68f.
Allocation sampling remains16515072 EXTENSION bytes on both engines, zero before/
after; this is not peak RSS or final performance acceptance.

Remaining: full directory APIs, configuration/protocol/secret/real-service edges,
region redirects/refresh/version pinning, HuggingFace/options/system roots/autoload,
remaining core gaps and every final common-source platform/client/SQL/shell/CAPI/
ADBC/DB/WAL/coupled speed/RSS acceptance gate. The extra prepared EXECUTE listing
remains explicit. See `docs/httpfs-s3-partition-delete-parity.md`; the full goal is
active and incomplete. Required client/platform scope and accepted memory policy
are unchanged.

Earlier write checkpoint follows; the partition/delete checkpoint above is current.

S3 COPY writes and explicit upload finalization are implemented (review691).
Signed Mac ARM64 image685 matches C++ on 35 functional SQL records; a separate
blocked-PUT allocation sample is16515072 EXTENSION bytes on each engine, returning
to0 before/after. Single PUT, multipart, retries, KMS headers, CSV/BLOB/gzip/Parquet,
failure diagnostics and object visibility are covered. Raw parts use the existing
buffer manager; ordinary flushes do not finalize and workers are joined. No memory
budget, table/query cache or release_on_unpin policy changed.

CSV/BLOB/gzip bytes match. Parquet bytes differ (Cpp904/Rust635), but independent
opposite-engine reads of four actual published files per direction match695.
This is payload compatibility evidence, not DuckDB DB/WAL acceptance. Allocation
sampling is not a peak-RSS/coupled-throughput benchmark or final acceptance.

The same image retains30 S3 read/list/limit cases /276 literal records687,16 HTTP
cases /121 records688,59 extension cases /246 records689 and full public C API690:
30 files /155 cases /724158 assertions. Strict workspace/all-target Clippy683 passes.
Image685 SHA256:8cc0c0374faa38336b8704582b435cc69dbb2408aa0de203eee476479eee766a.
Fresh build684, strict normal signing and whole-file pinned CMS verified before/
after. Original compile/lint and missing-setting before682 failures are retained.

Remaining: deletion/partitioned COPY and remaining upload configuration/protocol
edges, region redirects/refresh/version pinning, real service coverage, HuggingFace/
options/system roots/autoload/startup, remaining core gaps and every final common-
source platform/client/SQL/shell/CAPI/ADBC/DB/WAL/coupled speed/RSS acceptance gate.
The known extra listing per Rust prepared EXECUTE remains explicit; no cache was
added to remove it. This is component evidence, not full S3/HTTPFS or release
completion. See `docs/httpfs-s3-write-parity.md` and external reviews691/694/695.

Earlier listing checkpoint follows; the write checkpoint above is current.

S3 listing and incremental glob scanning are implemented (review671).
Signed Mac ARM64 image666 matches C++ in 30 cases / 276 literal SQL records.
ListObjectsV2 pagination, encoded keys/tokens, recursive patterns, directory-marker
exclusion and C++'s density-based traversal switch are covered. Optional registered
GlobResult preserves page boundaries: LIMIT0/1, prepared re-execution, pattern lists
and late errors now match. Before663 preserves the real eager-binding LIMIT1 failure.
No object-body/query/table cache or memory-budget change was introduced.

The same image retains 16 HTTP cases / 121 records668, 59 extension cases / 246
records669, and full public C API670: 30 files / 155 cases / 704444 assertions.
Strict workspace/all-target Clippy664 passes. Native image666 SHA256:
9d7aa2ccc6477d4475dc77e92ac169fc4f24737967eab5275ca6b5210dbef790.
Strict native signing and whole-file pinned CMS authentication verified before/after.

One request-count distinction remains explicit: the prepared/limit group makes
8 Rust listing requests versus 6 C++; probe673/674 identifies one extra request
per Rust EXECUTE. All SQL/results/errors match. No cache was added to close this
small difference; performance disposition remains open (receipt675).

Remaining: multipart writes/deletion, region redirects, refresh/version pinning,
full remote protocol/service and reader lifecycle coverage, HuggingFace/options/
system roots/autoload/startup, remaining core gaps and every final common-source
platform/client/SQL/shell/CAPI/ADBC/storage/WAL/coupled speed/RSS acceptance gate.
This is component evidence, not full S3/HTTPFS or release completion. See
`docs/httpfs-s3-listing-parity.md`; original failures and before images are retained.

Earlier read checkpoint follows; the listing checkpoint above is current.

S3 exact-object reads and config secrets are now implemented (review644).
Signed Mac ARM64 image637 matches C++ in21 cases/173 SQL records against an
independent loopback SigV4 verifier. Includes scoped s3/aws/r2/gcs secrets, session
and requester tokens, aliases, encoded keys, environment/global configuration,
HTTP-secret fallback and literal wildcard keys; CSV/Parquet/read_blob answers
asserted. Generic signed-request redirects are disabled pending explicit re-signing.
No body/query/table cache or memory-policy change.

The same image preserves16 HTTP controls/121 SQL records641,59 existing extension
cases/246 records639, and full public C API642:30 files/155 cases/825996 assertions.
Strict workspace/all-target Clippy635 passes. Native637 SHA256
73dc91fe884aa0bcf619e861ed38c200feee3443357b8c74955f2e8ced8450a6.
Normal strict signing and whole-file pinned CMS checks before/after. Earlier
compile/lint/header propagation and harness/comparison errors are retained.

This is exact-object S3 read component evidence, not complete S3/HTTPFS or release
acceptance. Multipart writes/listing/deletion, region redirects, refresh/version
pinning, wider service/error controls, HuggingFace, remaining options/system roots,
autoload/startup and remaining core edges are open. Required final common-source
SQL/shell/CAPI/ADBC/Python/Go/Swift/Linux/iOS/storage/WAL/coupled speed/RSS gates
remain open. Existing targeted client evidence predates this image. See
`docs/httpfs-s3-read-parity.md` and external `s3-read-review-644/verification.json`.

Earlier component checkpoints below are historical and do not supersede this frontier.

SQL HTTP(S) activation and scoped HTTP secrets are now wired (review621).
The existing loader recognizes only the pinned httpfs4bc690d package for engine
v1.5.6 and the current platform. It performs the existing metadata/signature and
named-install-info checks, then registers the Rust HTTP(S) implementation and
records loaded state. The C++ package is data-only: its machine code is never
opened/executed. This is not general C++ extension-ABI support or complete HTTPFS.

Core HTTP secret lookup now reaches the provider: scoped bearer/extra headers,
proxy values and verify_ssl override. Debug formatting omits credential values.
The same portable public-C-API control618/619 matches C++ in16 cases/121 records620,
including INSTALL/named LOAD/repeat, TLS/CA/redirects/CSV/Parquet/ETag and scoped
secrets. Only known output/package/probe/TLS paths, loopback origins and target
platform components are mapped; errors, values and rows are compared literally.
Four policy cases599 verify no provider registration on tamper/unsupported revision/
metadata mismatch, plus repaired-info retry, repeat LOAD and loaded catalog state.

Targeted REAL primary clients now pass normal SQL INSTALL/LOAD, HTTPS CSVsum3,
Parquetsum4950 and scoped-bearersum3. Python CP313 native601+normal facade612 passes
613; pinned Go605 and Swift609 consumers pass610, each verifying its actual native
image. This is targeted Mac ARM64 integration, not full-client/ABI/platform gates.
Python600 lacked direct-Cargo macOS linker flags;601 corrects them. Python602's
manual prefix lacked dist-info;612 adds correct project metadata with byte-identical
native code, without changing Python source. Swift607 needed header padding;609
corrects it. Preauth608 stopped before execution. All failures are retained.

Strict workspace/all-target Clippy598 passes. Signed shared594 retains all59
existing extension cases/246 records615. Full public C API616 passes30 files/155
cases/725186 assertions; SHA256
22a7887c72550e77e82b093df4b56e5a694f239ea1d7e3db61716d9d9f7f6630.
Source622 archives this development state. Cache and memory-budget policy unchanged.

Remaining: S3/HuggingFace and their secret providers, missing HTTPFS options/full
protocol/system-root behavior, autoload/startup configuration, EXPLAIN ANALYZE and
remaining extension edges. Full selected-client suites/ABI matrix, Linux/iOS and
final common-source SQL/shell/CAPI/ADBC/storage/WAL/coupled speed/RSS acceptance
remain open. Do not relabel this milestone as full HTTPFS or release completion.

Native HTTP(S) provider component is implemented and verified through explicit
SDK registration (review587). New crates/httpfs uses pinned UREQ3.5/rustls/ring
with per-handle agents and positional reads; no response-body/query/table cache
was added. Database::register_http_file_system is an embedding bootstrap, NOT a
complete httpfs LOAD implementation. SQL LOAD httpfs, primary-client activation,
S3/HuggingFace/secrets and the remaining HTTPFS contract are still open.

Actual C++560 and signed native RustSDK578/580 match eleven loopback-TLS controls:
eight successful outcomes and three rejection diagnostics (only the known
loopback origin is mapped). CSV/Parquet reads, custom CA trust, verification modes,
redirects, missing file and HTTPS extension INSTALL/LOAD are covered. Typed TLS
errors and GET/404 status diagnostics now match; older failures/differences remain
preserved. New reusable server control: scripts/httpfs-tests/tls_control.py.

Two genuine correctness gaps were found and fixed. read_blob/read_text now use
registered handles for content/size/mtime, preserving requested-column reads and
callback errors. Optional registered FileHandle.version_tag carries HTTP ETag into
the EXISTING external-cache validation. At unchanged size/mtime, CSV sum3→7 and
filtered Parquet sum4950→14950/count100 now match C++; before566 incorrectly gave
NULL/0. SDK assertions enforce the updated answers. Cache budgets and the
release_on_unpin/lower-memory policy remain unchanged.

Strict workspace/all-target Clippy582 passes. Current signed shared579 preserves
all59 existing extension cases/246 records585; the rebuilt SDK584 preserves seven
registered-source cases literally. Full public C API586 passes30 files/155 cases/
762874 assertions; native SHA256
1103a4e678cf751b4e37c724e5a6d53c23a22605a935506e29aaf9327ed9550f.
Source588 archives current development inputs. No final release freeze or broader
platform/client/SQL/shell/storage/WAL/performance acceptance follows from this.

Remaining provider work includes actual SQL/client integration, full setting and
secret/S3/HuggingFace registration, system trust-root parity, broader URL/header/
range/redirect/retry/metadata/error contracts and required target builds. Final
common-source selected-client/platform/storage/WAL/coupled speed/RSS gates remain
open. The complete original goal and accepted performance tradeoff are unchanged.

Registered remote extension sources now reach their filesystem backend (review545).
Direct INSTALL previously opened HTTPS sources with std::fs::File even when a
filesystem had been registered; signed before533 fails with NoSuchFile. The fix
uses the existing DatabaseFileSystem/StorageFile boundary for recognized remote
schemes, including existence checks, .gz fallback, read/seek, callback errors and
disabled-filesystem policy. Local/unknown protocols retain C++'s local path. Input
bytes still stream through the existing validation/signature/publication helper.
No file snapshot or query/table cache was added.

The same seven SDK/control cases on signed Rust543/544 and C++529/530 match
literally: raw/gzip/fallback INSTALL+LOAD, missing source, callback read error,
disabled filesystem and unknown protocol. Every successful installed body equals
the authenticated fixture before LOAD. The signed shared537 image also passes
all59 existing extension cases/246 records540. Strict full-workspace Clippy541
passes. Full public C API539 passes30 files/155 cases/705148 assertions on that
image; native SHA256 51a7ca39837ff59b29b42d67081e840e649310b105913fe117cf7f2834c5cbd8.
Source546 archives this development state; it is not a final release freeze.

This fixes the dispatch prerequisite for HTTPS/httpfs and registered client
filesystems. It does NOT implement built-in TLS/httpfs. Official pinned Linux
httpfs519 was separately CMS sealed and loaded by C++521 with unsigned/community
extensions disabled; footer version4bc690d maps to source commit
4bc690dba4496c765777a0269d48fdbaff7cdc11 (source522). The actual reference adds34
settings and no SQL functions; source registers HTTP/S3/HuggingFace filesystems
and secret providers. Default Curl certificate verification is true; the older
httplib setting defaults false. Use the pinned source and actual TLS controls,
not the current stable documentation redirect (which now points to2.0).

Next: implement the native HTTP(S) provider/TLS and its range/CA/redirect/secret/
configuration behavior, then S3/HuggingFace and remaining extension/utility edges.
Final common-source platforms/selected clients/SQL/shell/CAPI/ADBC/storage/WAL and
coupled speed/RSS acceptance remain open. Lower-memory policy is unchanged.

Conditional extension updates and local gzip parity are implemented in the current
development image509 (review513). Local INSTALL now uses C++'s first gzip member,
checksum/header behavior:10 cases/28 records match the reference. UPDATE EXTENSIONS
now binds, plans and executes through the ordinary direct/prepared query path,
returns the five C++ VARCHAR columns and statement type, uses If-None-Match only
for updates, preserves installed files on304 or failure, and reports the C++
version/update tags. Fourteen cases/40 records match, including missing/custom/
corrupt metadata, new/same/empty versions, HTTP failure and prepared execution.

These controls exposed two result-metadata defects, now fixed: direct LOAD/INSTALL
must retain the empty Success BOOLEAN column, and completed prepared results must
retain their statement type. Three serialized-plan and two plain EXPLAIN records
also match C++ literally, without creating the extension directory. Strict full
workspace/all-target Clippy511 passes. All four extension regression scripts on
signed509 match59 cases/246 records, plus the five plan records (251 total)513.
No values/errors/statuses/types/request sequences are normalized; only known test
paths, loopback origins and target-platform path/documentation fields are mapped.

Full original public C API505 PASS on the same signed Mac ARM64 development509:
30 files,155 cases,789266 assertions. Library SHA256
6b78660098df610db81166939df18c0286b2a11ecae35fc7393d507db661944f.
Source515 archives the updated development inputs. No heavy jobs remain.
This is not a final release freeze or all-platform acceptance.

This is development component evidence. HTTPS/httpfs, EXPLAIN ANALYZE utility
execution, further multiple-directory/name/metadata/version/nesting/protocol edges,
and final common-source platform/client/SQL/shell/CAPI/ADBC/persistence/WAL and
coupled speed/RSS acceptance remain open. No cache or memory-budget policy changed.

Initial built-in HTTP installation is implemented (checkpoint479). The signed
Rust456 before-image fails a valid gzip HTTP INSTALL as NotImplemented467.
The new transport handles Content-Length, chunked and connection-close framing,
C++ retry statuses/counts, nonredirecting requests, proxy/basic authentication and
path escaping. HTTP gzip decoding follows C++'s first-member semantics. Downloads
stream to temporary files, then use the shared metadata/signature validation and
publication path. ETag field104 is retained in install metadata. Repeated INSTALL
avoids a request; failed FORCE preserves the existing body and metadata; HTTP304
with existing metadata preserves them too.

The same repository regression477/478 matches C++ in25 cases/105 records and all
request sequences479, including truncated transfers, exhausted retries, gzip
errors/CRC/concatenation, unsigned-policy rejection and a path containing spaces.
Twelve binary .info files match after replacing same-length test-server origins.
Only known output paths, loopback origins and target platform fields are mapped;
raw errors/statuses/values are otherwise compared. Failed before-image467,
path-escaping failure474 and initial comparison471 are preserved. Strict workspace
Clippy482 passes after correcting a documentation lint480.

Fresh signed native Mac ARM64 development484 passes full public C API suite481:
30 files,155 cases,747364 assertions. Library SHA256
02a895b215d260be1d63e4730a0e35b357d9573b14ddb041d492606e91406e7e.
Local/search regression489 also matches all10 cases/73 records against C++457,
including local INSTALL through the refactored staging helper. Source487 archives
this development state; it is not a final release freeze. No heavy jobs remain.

This older checkpoint covered initial plain HTTP installation. The later progress
above adds conditional UPDATE and local gzip parity. HTTPS/httpfs and the broader
remaining contract/release gates are still open.
No query/table caching or memory-budget policy changed.

Ordered extension lookup now follows C++ (checkpoint461). The signed Rust437
before-image fails when only the second configured directory contains an extension
(450). The implementation preserves the custom directory followed by the full
extension_directories list, including empty entries; LOAD selects the first regular
file, INSTALL writes to the first directory, and duckdb_extensions() scans every
directory in order. A directory named *.duckdb_extension is skipped during LOAD.
The initial452 build's non-file failure454 is retained; fresh signed456 fixes it.
The same repository public-C regression457/458 matches C++ in all10 cases and73
records, including catalog paths before/after loading and raw error strings after
only known test-path/platform mapping459. Workspace/all-target strictClippy460
passes. Signed Mac ARM64 full public C API suite462 passes30 files/155 cases/
723888 assertions (pending/streaming loop counts vary). Source464 archives the
development inputs. Final release/client/storage/performance acceptance is still
open. Network INSTALL remains incomplete.

Lifetime checkpoint444 closes a confirmed result-retention difference. Eight
concurrent C-ABI LOADs initialize once in both engines. C++433 frees scalar user
data when the retained prepared statement is destroyed, even while a materialized
result remains readable; Rust416/435 incorrectly kept the connection alive until
result destruction. ResultData now retains a connection only for streaming
results, releasing it after materialization or failure. Fresh signed Rust437/438
matches all six C++ counter stages exactly. The same public-C regression442 passes
four cases on both engines: materialized result, streamed fetch, stream converted
through the value API, and unread-stream destruction. Strict workspace/all-target
Clippy443 passes. Signed macOS arm64 full public C API suite445 also passes:
30 files,155 cases,770834 assertions. Source447 archives these development inputs.
These are development proofs, not final release acceptance.
The uncached lower-memory policy and memory budgets are unchanged.

Layout/signature checkpoint430 adds ten independent C layout compilations across
all five required target slices for the404-field v1.2.0 prefix and546-field full
table. Official core-signed inet bytes are accepted and a one-bit mutation rejected
with unsigned/community permissions disabled. Rust's data-only test explicitly
allows platform metadata mismatch to validate a Linux file on macOS; no downloaded
code ran. These do not close signed-C-ABI runtime or final platform/client gates.

Shared-library checkpoint420 adds actual public C API INSTALL/LOAD control402
and exact metadata-error408/initializer-error418 comparisons with C++ oracles.
It fixes validated string-option boolean handling, required .info mode fields and
initializer error categories. Workspace/all-target strict Clippy419 passes. These
remain development components; broad signed/version/lifetime/ABI/platform/client/
release/performance acceptance is still open.

Implementation checkpoint: the live bridge now passes native macOS SDK control360
for explicit unsigned loading (scalar/NULL/vector/repeated LOAD/catalog/reconnect,
two independent databases) and rejects the fixture with unsigned loading disabled.
Signed native unit355 passes independent OpenSSL signature/tampering/trust and
version checks; workspace/all-target strict Clippy361 passes. These are development
components. Initial local INSTALL now also passes signed native379 and exact C++
.info bytes373; latest workspace Clippy382 passes. Initial force/origin/gzip/permission-mask controls now pass385/391, matching file
modes0666 filtered by umask and directory0755 filtered by umask393; workspace
Clippy392 passes. Broader metadata/signature/lifetime controls and
shared-library/selected-client/platform refreshes,
complete metadata/signature/lifetime controls and final common-source release/
performance gates remain open. Source226 receipts remain before-image evidence.

**Origin of the current extension work:** control341 exposed the missing stable
C-ABI loader in the Rust228 before-image. The live bridge now loads C_STRUCT
extensions, with the scoped verification above. C++ binary ABI exclusions remain
separate. Complete network INSTALL and remaining metadata/version/nesting
contracts, then freeze/rebase final common-source gates. Old queues remain
deferred321/343; source226 receipts remain before-image evidence. See
[the scoped extension status](../conformance/divergences.md).

The frozen source226 contains the nonrecursive temporary-directory fix, explicit
Unix0755 permissions, C++ OS-error text/errno metadata, and the Pandas3 `str` binder
correction. Its2320 source inputs are archived on the external SSD. Signed buffer
verification227 passes strict Clippy, formatting,21 unit and11 integration tests
under umask000. The signed before185 binary fails the original new missing-parent
regression. Public before222 confirms the old engine created missing parents,
requested0777 under umask000, and returned different existing-file errors.
Optimized native228 has compiled freshly and passed signing, all30 C API files /
155 cases /924694 assertions, and547 public-export checks. Public229 matches five paired scenarios (ten executions) literally: status, raw error bytes/errno, full generation counts,
umask000/022/077 modes and cleanup/user-file effects. Original unchanged
`dbgen_error.test`230 passes both engines in fresh roots without creating parents.
The corrected full corpus has completed with stable postchecks342; the newly
confirmed C-ABI loader defect and remaining release gates are still open.
The signed CP313 Linux x86_64 wheel235 is built from the same immutable source226
and installed normally in fresh Arrow19/21 environments. Original238 matches all
eight Pandas3 cases per profile; expanded239 matches22 Arrow19,22 Arrow21 and14
real-no-Arrow cases per engine, including retained owners and copy-on-write
snapshots. All four complete full-fast comparisons241/242 now pass: baseline and
Pandas3 on both Arrow19/21, with3241/3492 records per engine and exact case IDs,
verdicts and skip reasons. All nine supplements244 also pass: packaging/typing/
installed ADBC, object/non-fast, eight typed checkpoint/WAL journeys,48 encrypted-
Parquet cases, producers, named memory, Arrow-string lifetime and client metrics.
Native ADBC247 now passes all64 cases/3040 assertions per engine and the public
shipping-library consumer. Go251 passes all652 cases per engine plus four typed
checkpoint/WAL journeys252. Swift254 passes132 upstream plus one identity case
per engine, followed by four typed consumer journeys255. Shell258 passes392
upstream cases with three identical skips; all768 differential/46PTY cases retain
four documented build-identity differences and one native-frame exception after
manual review. These results are scoped to source226 Linux x86_64 under Rosetta.
Other ABI/platform/storage/performance release gates remain open. Client medians and explicit RSS excess are recorded in external253;
they are Rosetta diagnostics, not native TPC-H/ClickBench acceptance.

The corrected full paired corpus260 is running in separate pristine engine roots,
with the original4870 SQL/1522 data files,300-second bounds and7GiB/no-swap limit.
Both engine collections and their postchecks are complete at11:56 UTC. C++ has
4,793 passes,75 skips,one gzip memory-limit crash and one VARIANT TPC-H timeout.
Rust has4,780 passes,75 skips,14 documented failures,one matching gzip OOM and
no timeouts. All4,870 IDs and75 literal skip reasons match; original4,870 SQL/
1,522 data and signed native/reference assets verify before/after. dbgen_error
passes both fresh roots and leaves missing parents absent. Original failures are
retained; collection success does not accept them or close the newly confirmed
C-ABI loading gap. Audit342 preserves the exact identities and scope.

CP310263–266, Apple277, full iOS simulator282 and subsequent Mac client/control
queues were unstarted and are now deferred321/343 because the confirmed core gap
requires a fresh integrated source. Their scripts/inputs/receipts remain preserved;
none is a passing gate. The owned simulator uses the authorized internal disk;
builds and evidence remain on the SSD. No Mac Intel target is required.

The complete before-image140 corpus is4779PASS/75SKIP/15FAIL/1gzip memory-limit
crash, no timeouts, across4870 unchanged SQL files. C++ is4791PASS/75SKIP/1crash/
2timeouts/1failure. Every Rust skip matches C++'s literal reason. The reused-root
C++ `dbgen_error` failure is fixture pollution: fresh188 gives C++PASS/RustFAIL,
with only Rust creating the missing parent. This is a real defect, not a lower-
memory waiver. Raw results, shared nonpasses and original failures remain intact;
new full verification must use the prepared separate pristine engine roots191.

Pandas3 before177 confirms four `str` failures on Rust versus correct C++ values.
The expanded reference224 controls pass all22 Arrow and14 actual-no-Arrow cases
on C++, including NULL/NUL, copy-on-write snapshots, mutation and retained owners.
The old Rust135 wheel still differs. Both original and expanded controls now pass on the new235 wheel as recorded
above. Complete client suites, supplements, other ABI/target cells and final
performance acceptance remain required.
Original218/223 controls retain their incorrect mutation expectation and failed
reference evidence;224 corrects it to the observed C++ snapshot contract without
removing value, mutation or lifetime assertions.

A shared Cargo target reused the old test executable across snapshot mounts in
failed219. Its source hashes/signature were valid but did not bind that cached
payload to the new source. The original failed receipt is preserved; clean221
and227 compile test payloads freshly and pass. Candidate228 likewise uses its own
new target and requires freshly compiled public payloads before freeze/signing.
Only immutable source226 is compiled; later documentation updates record evidence.
No SQL, ratchet, table-cache or memory-budget policy was weakened.

Evidence: external `platform-matrix-20261029/` raw audit216, before probes188/222,
Pandas controls224, source226, buffer227 and pending native228/public229/SQL230.
The component results below retain their original source/artifact scopes; final
common-source SQL, clients, storage/WAL and coupled performance gates remain open.

The signed Linux x86_64 source-46 library passes all 30 original C API files,
155 cases and 947,474 assertions, with 547 shipping exports. NULL validity and
streaming repairs remain green. Container and job-allocation memory defaults now
match C++ in all five live controls; the 7 GiB container reports 5.5 GiB instead
of Rust's previous 6.1 GiB. This reduces the default budget and introduces no table
cache. See [memory limits](linux-process-memory-parity.md) and
[C API repairs](capi-null-stream-parity.md).

Both signed engines pass 392 applicable upstream Linux shell tests, with the same
three skips. The source113 CLI now completes all768 differential cases/46PTY with zero
unexpected behavioral differences, four explicit build-identity cases and one
scoped native-frame exception. Earlier failures remain preserved; see
[the exact shell component boundary](linux-shell-parity.md). Final all-artifact
verification remains separate.
[Linux x86_64 evidence](linux-x86_64-native-preflight.md) retains the earlier
4,870-file SQL result, shared resource failures, failed ratchet and 21 individual
whole-file timing outliers. It is translated functional execution, not native
x86_64 hardware performance evidence.

All three required Apple arm64 slices were refreshed on source 43. Mac arm64
passes all 155 C API cases. The signed embedded simulator framework passes all
132 upstream Swift tests plus one loaded-image identity test on iOS 26.5.
Earlier typed consumers and four cross-engine checkpoint/nonempty-WAL journeys
also pass. Physical-device execution/provisioning, wider iOS core/C API/ADBC and
final common-source verification remain open. See [Apple packaging](apple-swift-package.md).

The required Python matrix has 15 ABI/target cells: CP310 through CP314 on
Mac arm64, Linux arm64 and Linux x86_64. Original C++ controls for all 15 cells
are downloaded, hash/architecture checked and content-signed. That is input
preparation, not Rust client acceptance. Only the stated earlier Mac/Linux
arm64 CP313 component gates are verified. See [Python ABI coverage](python-release-abi-matrix.md).
The refreshed [Linux x86_64 Go component](linux-x86_64-go-client-parity.md) now
passes all 652 paired cases and four independent typed checkpoint/WAL journeys.
Historical Linux arm64 Go 652, Swift 132+identity and ADBC 64-case results remain
component evidence on their recorded artifacts.

Completion still requires closing remaining gaps and verifying final signed
common-source artifacts across SQL, shell, C API/ADBC, selected clients and
cross-engine persistence/WAL, with coupled native TPC-H/ClickBench speed/RSS and
individual outlier dispositions. Preserve the accepted lower-memory policy and
COPY exception; historical performance does not accept newer artifacts.

## Preserved integrated checkpoint — 2026-10-29

Updated 2026-10-29. **The full port goal remains active.** The current signed native
candidate is `art-resident-checkpoint-20261029/source-1/manifest.json`: 969 overlay
entries / 967 guarded inputs. Native expression/CREATE INDEX/ALTER PRIMARY KEY WAL
buffers restore without table rebuilds when metadata is available. Constraint storage
order handles ALTER/rename names, and checkpoints keep untouched deprecated-format
subtrees unloaded. No table cache or higher memory budget is introduced.
See [current persistence/recovery report](art-resident-checkpoint-parity.md).

Current signed checks: Clippy; nine SDK tests per debug/release profile; 141 broader
tests including 23 index tests, all without ignores. Native C++/Rust cold recovery
and checkpoint reopen pass at 1 MB, including two-million-row plain/expression index
checkpoints with spill disabled. The **full 4,870-file corpus has 4,781 passes,
74 exact C++ skips, 15 documented exceptions, and zero crashes/timeouts/regressions**.
All 1,522 fixture hashes and every verdict/skip reason are stable. C API **155 cases /
728,156 assertions** and **52 cross-engine recovery journeys** pass on the same images.

Current native SHA256 values:

- Library: `01aaeb24d068580f7755656062ba7a3e8a31bbbdfd4a1c5933379a59ecb0a702`
- Shell: `d53e9768c456ff13d0327c0c9960e485dc1ff9f40af9223cf539c24a4a90a5b7`
- SQL runner: `5942f2cfd1c6f970e10350bf6d8afb59f33f56c965f87fedc4470c01344ada5d`

Strict signatures, actual loaded images, all 547 C API exports and absence of ADBC
test hooks are verified. Three-repetition recovery measurements reduce Rust's close
cost from roughly 82 ms to 18 ms and process RSS from 75 MiB to 61–62 MiB; C++ remains
about 4.4 ms / 59 MiB. The remaining close outlier is explicit and separate from final
TPC-H/ClickBench acceptance. Full workspace unit/integration verification now passes 1,001 tests across 141 signed
executables, with seven existing diagnostics/probes ignored. Shell verification passes
392 tests/three shared skips and 768 differential cases including 46 PTY sessions,
with zero unexpected differences. Current Python Arrow19/21 fast matrices exactly match
C++; non-fast71/eight skips, installed ADBC, typing and eight 15-column file/WAL
journeys pass. Go652 plus an external consumer/four journeys and Swift132+identity,
relocated/standalone distribution/four journeys pass. Native ADBC64/3040 passes.
See [current client evidence](final-client-verification-20261029.md). All130 final native TPC-H/ClickBench configurations pass; geometric means preserve
the accepted tradeoff, with weighted sums/peak RSS/all63 outliers explicit. See
[final native performance](performance-final-native-20261029.md). The current Python-only
object-buffer candidate is rebuilding separately; native core bytes remain unchanged.
Platform and broader contract acceptance remain open.
Shared reference limitations remain explicit in [divergences](../conformance/divergences.md).

Retained dictionary source4 evidence: original 10/100 MB no-spill COPY budgets, four
SDK tests per profile, 202 broader tests (two optional tools ignored), 693 SQL files
and 12 cross-engine Parquet files/reads passed. The current ART overlay preserves
that repair. See [dictionary repair](parquet-dictionary-memory-parity.md).

The installed Python wheel now uses Resident source1, including dictionary/ART fixes.
Its signed extension is `3b9c8b632ee1055cd7f671b36ac765c5718a929350979fe41ff7aca6666f0ff9`.
Both complete fast matrices and62 additional producer controls per Arrow environment
pass. Final client string scans retain explicit1.73x/1.92x time outliers; see
[the current report](final-client-verification-20261029.md). Earlier Arrow source7
receipts remain component history in [Arrow report](arrow-producer-parity.md).

Remaining observable core/memory audits and full final same-source SQL, unit, shell,
C API/ADBC, Python/Go/Swift, persistence/WAL and paired speed/memory verification remain
open. Preserve the accepted lower-memory baseline and COPY tradeoff; report query
outliers. No exact performance parity, outperformance or additional clients are needed.

The earlier aggregate checkpoint below is retained component evidence.

## Current aggregate/native integration checkpoint

| Gate | Actual current-source evidence |
|---|---|
| Workspace/all-target Clippy | Pass, warnings denied (`clippy-5-terminal.json`) |
| Signed targeted SDK debug/release |36 complete frozen C++ physical plans/values/raw names plus eight return-type controls each |
| Signed broader affected crate tests |142pass across22 executables; one existing optional full cast-oracle CLI tool ignored |
| Signed native images | Actual build exited0; frozen CLI/library/SQL runner strictly signed;547 C API exports/zero ADBC test hooks |
| Public C ABI native matrix |50 complete physical/serialized plans, values, metadata, empty/frame/ordered/callback/extra-data controls match C++ |
| Checked reference defect | Three multi-row ordered C API calls receive invalid C++ state arrays; Rust correct sums and literal physical/serialized plans verified separately |
| Affected SQL |324 files:323pass/one exact C++ longdouble skip; zero failures/crashes/timeouts/verdict regressions |
| Full C API suite |30files/155cases/747436assertions pass; each loader selects the immutable current library |
| Full workspace unit/SQL/shell/clients/native ADBC/persistence/WAL/performance | Final refresh required; prior results are controls |

Current immutable library SHA256:
`e9b447335ee2c5fba4bcb399092d74676e85284a1acdfa769ecd0f2d62c2372d`.
CLI: `6798e2fec6736382a74900dfb43663a92e83b8be145e8cb82d1795cf1e5409e8`.
SQL runner: `846188c762c3801ae79a1f819bbff6ebe872f840473c1d524d07201e3e3dd301`.
Evidence: external `aggregate-equality-20261029/` and
[aggregate report](aggregate-equality-parity.md). The narrow C++ ordered-state
reference defect is explicit in `conformance/divergences.md`; the50 paired controls
remain literal comparisons, separate from reference-fault diagnostics.

Retained FSST component evidence:73 exact corpora/146 cross-decodes,124 signed
common/storage tests per profile,190 affected SQL files,155 C API cases and12
bidirectional FSST/DICT_FSST checkpoint/nonempty/torn-WAL mutation/readback journeys,
with20 immutable raw inputs. See [FSST report](fsst-parity.md). Earlier codecnative
controls remain external `zstd-strategy-parity-20261027/`:25/25 signed codec tests per
profile,2072 explicit frames and20 cross-engine Parquet files including70MiB LDM
values. See [codec report](zstd-strategy-parity.md). Actual SSD reads/writes succeed.

The confirmed EXPORT gaps were closed at the retained before-codec checkpoint:
160/160 complete public plans match C++; all14 signed SDK cases, eight cross-engine
exports/imports and installed Arrow19/21 registered filesystem checks pass. See
[EXPORT evidence](export-plan-parity.md). That component evidence does not
substitute for final current-source release verification.

Remaining contract audits in `work-queue.md` still need authoritative disposition:
ART legacy/expression/WAL paths, nested VARIANT statistics,
Parquet schema evolution, vector-heap accounting, configured extensions/platforms.
Verify each against actual current code/runtime; stale backlog prose alone neither
proves a gap nor closes it. Full final SQL/shell/selected-client/ADBC/persistence/WAL
and coupled TPC-H/ClickBench/outlier gates remain required. No goal completion is
claimed by this aggregate/native checkpoint.

## Retained before-codec integrated control

The following batch-copy source8 checkpoint predates the current native codec image.
It remains historical integrated evidence, not current-image release acceptance.

### Batch-copy source8 gates (before-codec)

| Source-8 checkpoint | Verified state |
|---|---|
| Workspace/all-target Clippy | Pass, warnings denied (`clippy-build-6`) |
| Signed ordered-COPY SDK | Four passes (`sdk-verification-4`), including pressure, ordering and failure recovery |
| Signed shipping artifacts | Strict signatures; 547 C API exports; zero ADBC test hooks (`native-images-2`) |
| C API | 30 files/155 cases/725,896 assertions; actual selected-library loader paths verified |
| Supplemental encrypted-Parquet SQL | Both tests pass on Rust and C++, including encrypted SF1 export/import and 22 TPC-H answers through tables/views |
| Coupled COPY diagnostics | 36 samples completed against C++ and preserved old Rust; all 33 successful files independently cross-read for every payload and order |
| Expanded affected SQL | 1,059 files: 1,047 pass, 11 exact C++ skips, one documented avoided-OOM assertion; all verdicts and failure payload unchanged |
| Full 4,870-file SQL / full unit suite | Fresh final-candidate verification still required |
| Shell upstream | Fresh Rust/C++ 392 passes/three exact shared skips using existing SSD lexical-alias fixture; initial shared unquoted-space-path failures retained |
| Shell differential/PTY | Full source-8 768 cases/46 PTY pass: zero unexpected differences, one known native-frame-body exception |
| Python Arrow 19/21 fast | 3,241/3,492 records, exact C++ IDs/verdicts/skip reasons; signed source-8 wheel installed in both environments |
| Python non-fast / installed ADBC | Fresh non-fast 71 pass/eight shared skips; both installed ADBC/file/UDF handoffs pass; 547 API exports/zero test hooks |
| Python persistence/WAL | Eight 15-column append/checkpoint/nonempty-WAL/mutation/readback journeys pass across both Arrow environments |
| Go | Complete pinned root suite with Arrow: 652 passes, exact C++ IDs/verdicts; 79 source files and actual selected DLL verified |
| Swift | Fresh Rust/C++ 132 upstream plus identity pass with exact IDs; relative framework, relocated/standalone consumers and four checkpoint/WAL journeys pass |
| Native ADBC | Fresh Rust/C++ full 64 cases/3,040 assertions each pass with exact ordered IDs; feature-free shipping restored/verified before harness; isolated bridges and feature Clippy verified |
| Native/client persistence/WAL | Refresh required on final images; prior scoped journeys remain controls |
| Full TPC-H/ClickBench timing/RSS/outliers | Final-candidate measurement and documented acceptance still required |

The immutable signed shipping library SHA-256 is
`2c96baa818920f6be19daaebea5ed3e3cbcf833a0a308139329ff0184bebca0c`.
The CLI is `6527e7f76b907296ff05c5f2a019407480065e04fb49c5561b554850a82a88ec`;
the SQL runner is `02e16760f696206437040c7104c239a67539150879417b0070f21f4f801e05a7`.
Evidence is under external `batch-copy-memory-20261027/`, including source, signatures,
SDK/C API, `encryption-sql-2`, `after-probe-2` and accepted `affected-sql-2` receipts. The source-8 Python native image is
`a6d88586d08536c79addc1d9545f891145165d876a8c7ce83df975b6293f4d14`.
Its fast/non-fast/installed-ADBC and persistence receipts are before-codec scoped evidence;
applicable broader contract/platform/extension audits remain.

COPY is an explicit performance outlier. At 14 threads/256 MB the corrected path is
303.0 ms/459.5 MB versus C++ 246.6 ms/463.8 MB, and the old unaccounted Rust path
87.4 ms/665.0 MB. At 512 MB it is 222.5 ms/565.0 MB versus C++ 152.9 ms/683.8 MB.
CPU concurrency follows C++ budgeted throttling. On2026-10-29 the owner explicitly
accepted this documented tradeoff, conditional on final TPC-H/ClickBench results
remaining within the approved speed–memory range. Individual outliers remain
visible, and final artifact measurements are still required. The first shipping
candidate's larger runtime regression remains rejected and preserved.
See [COPY measurements and lifecycle](batch-copy-memory-parity.md).

The accepted lower-memory TPC-H/ClickBench policy remains unchanged; no table cache
was introduced to close small speed gaps. Preserve individual query outliers and
whole-process plus interpreter-adjusted RSS. See [performance policy](performance-acceptance.md).
A narrow C++ uninitialized-VARIANT-padding raw-hash exception is documented in
`conformance/divergences.md`; defined payloads, normalized hashes and independent
file journeys have separate evidence in [nested hashing](nested-hash-parity.md).

## Before-stage complete integrated control

The following import-source-5 checkpoint predates encrypted-Parquet/nested-hash/COPY
changes. Its 909-entry/908-input overlay, full SQL/shell/client/runtime/packaging/
persistence and performance evidence remain preserved, and do not substitute for the
new candidate's final gates. Its Python image and all other identities below belong
to that before-stage checkpoint.

| Prior import-source-5 checkpoint | Verified result |
|---|---|
| Workspace/all-target Clippy | Pass, warnings denied |
| Signed import/registered-storage SDK | 15 pass |
| Targeted SQL | 785 files: 767 pass, two existing failures, 16 exact C++ skips; all prior verdicts unchanged |
| Full SQL corpus | 4,781 pass / 15 existing documented failures / 74 exact C++ skips; zero crashes/timeouts, zero ratchet regressions |
| Upstream shell | 392 pass, three unchanged skips |
| Shell differential | 768 cases / 46 PTY, zero unexpected differences; one native-frame-body exception |
| C API | 30 files / 155 cases / 744,074 assertions; selected signed library verified |
| Native ADBC | 64 cases / 3,040 assertions on both engines, same case order/adaptation hashes |
| Python Arrow 19 fast suite | 3,241 records: 3,205 pass, 27 skip, eight xfail, one existing XPASS; exact C++ IDs/verdicts/skip reasons |
| Python Arrow 21 fast suite | 3,492 records: 3,455 pass, 28 skip, eight xfail, one existing XPASS; exact C++ IDs/verdicts/skip reasons |
| Python installed import journeys | 14 literal C++ matches per Arrow environment, including registered-memory imports and failure/rollback behavior |
| Installed Python/ADBC | File/UDF/parameter/rollback/commit/reopen journeys pass; two additional import controls exact C++ per environment |
| Python runtime/stubs | 385 runtime exports / 29 typing-only declarations, presence exact C++; seven stubs and all 300 frozen test/data files verified |
| Go | 652 pass, exact frozen C++ case IDs/verdicts |
| Swift | 132 upstream plus one identity case pass on both engines; case IDs match |
| Swift distribution | Current relative framework, relocated package and standalone consumer pass with only the selected Rust image |
| Cross-engine persistence/WAL | Four scalar-expression, four Swift consumer and four 15-column Python append journeys pass; separate scoped cohorts |

The native shipping library is strictly signed and exports 547 C API functions with
zero ADBC test hooks. The ADBC test feature was copied to a separate library; the
feature-free shipping build was restored and checked **before** its harness ran.
The immutable shipping library SHA-256 is
`38603874f849b13c24ee23ec8cd6107294860c602bda19d63d3ad87bf8df622a`.
The signed Python image SHA-256 is
`2d7481420e5015748dea8ef27bca89fc3972c721103b9b4253cef60868bfa05b`.

The integrated evidence index is external
`import-preprocessing-20261027/verification-checkpoint-2.json`, including the accepted full-corpus receipt.
The full corpus has its own immutable runner/source/reference/fixture manifest and logs
under `full-corpus-1/`. The 15-column journeys preserve database and nonempty-WAL snapshots
before replay and verify the actual native image before/after every child process.

This is current-host macOS arm64 evidence. It does not silently close broader API
signature/overload/lifetime/concurrency/backend/platform audits, configured-extension
coverage, or earlier explicitly documented reference defects. Shared skips do not prove
unavailable extension functionality. Later catalog numeric dependency/registration-order
differences remain documented separately; startup main/schema identity and the literal
shell database-list difference are closed. The obsolete OID waiver is removed.

The full corpus has exact file coverage and every prior verdict is unchanged. All failure
payloads match except two occurrences of the generated worker-directory path in the
documented avoided-OOM caching-operators case; the exact mapping and original rejected
strict receipt are preserved. All 908 source/runner/reference/ratchet hashes, signatures
and 4,870 SQL/1,522 data fixture identities verify unchanged.

Reconcile remaining contract evidence and exceptions, and record coupled
TPC-H/ClickBench timing and memory against the preserved
uncached baseline and C++, with individual query outliers. Exact performance parity or
outperformance is not required. No table cache was introduced to erase small speed gaps.


The source-5 performance checkpoint now checks all 130 TPC-H/ClickBench query/thread
configurations and fits the approved aggregate policy; see [measured performance](performance-acceptance.md).
That before-stage checkpoint predates the encrypted-Parquet, nested-hash and ordered-COPY
changes now under verification. Supported footer-key encryption is implemented and has
independent cross-read/corruption/SQL evidence; column_keys retains the shared C++ limit.
The newer core/client/final-performance gates below must be completed on their own images.

## Upstream shell path calibration and current pass

The first source-8 upstream run produced nine failures because the upstream tests
interpolate temporary filenames into unquoted dot commands, while the physical SSD
path contains spaces. C++ reproduces the identical 395-case verdict set and the
same nine failures. That counterexample is retained separately.

Using the repository's existing `shell_ssd_tmp_path` fixture, both engines now
pass all 392 upstream cases with the same three skips and literal skip reasons.
The fixture delegates pytest's original lifecycle and returns only a whitespace-free
workspace symlink path to the same verified external storage. No test body,
assertion or shell tokenization changed. JUnit collection-path prefixes are mapped
only before the fixed `tools.shell.tests` module names; case names/verdicts remain
unchanged. All 909 source inputs, native CLI hashes and signatures verify stable.

Evidence is external `batch-copy-memory-20261027/{shell-upstream-1/path-control-comparison.json,
shell-upstream-alias-verification-2.json,shell-upstream-rust-alias-2/,
shell-upstream-cpp-alias-2/}`. Full 768-case/46-PTY differential verification now passes on the source-8 CLI
with zero unexpected differences and one native-frame-body exception. Case/driver
hashes, all 909 inputs and both CLI signatures verify unchanged.

## Next confirmed functionality gap

A fresh public C++/Rust probe confirms that CSV/Parquet EXPORT logical plans
serialize successfully on C++ while Rust returns NotImplemented. See
[EXPORT plan contract](export-plan-parity.md). The green source-8 component/client/
shell gates do not establish whole-port completion while that supported gap and
other contract/final-performance gates remain open.
