Current disposition, November 6, 2026: the owner explicitly approved a physical-device test exception. The goal is complete under that scope: all 22 scoped software collection/review requirements are evidenced; actual iPhone/iPad execution and provisioning remain **unverified, excepted by the owner**. Device builds, signatures and simulator tests retain their verified status. The [delivery index](release-candidate-index-20261106.md) links the new signed disposition, unchanged archive and all known limitations. Older dated statuses below are historical.

Current continuation (November5,2026): source2551 and signedMaccore/tools2556 are authenticated. Full4870MacSQL2560/2561:4826RustPASS/30SKIP/14documentedrawFAIL vs4839CppPASS/30SKIP/1referenceFAIL; all75nonpasses reviewed,0unknown/0skipdiff,44individualfiletimingoutliers. Current publicCAPI2582passed30harnesses155cases747170actualassertions (pollingvariance2592); nativeADBC2600passed64cases3040assertions each; Go2607/2609passed652cases each plus4typedWALjourneys; Swift2610passed133cases each. AllfivefreshnormalMacPythonABIs310–3142599/2649are independentlyverified:31135matchingXML outcomes each,30743PASS392retainednonpasses,36typedWALjourneys216encryptedParquet18installedADBCenvironments40typingruns18clientworkloads108samples. The full149workspacebuild2612/review2613 is active; component132passes/2diagnosticignores do not replace it. Originalignoredfunctionalcontrols,focused/CAST,shell,AppleSDK/simulator then finalLinux/storage/macro/composition gates remain queued or required. PhysicaliOS remains unavailable/unverified. No finalrelease or performance acceptance. Historical source2253 evidence below retains its original scope.

Current verification as of November 4, 2026 uses frozen source 2253, signed Mac artifacts 2268 and signed Linux artifacts 2255. The Mac SQL review covers all 4,870 original files: Rust has 4,826 passes, 30 skips and 14 documented raw failures; the unchanged C++ reference has 4,839 passes, 30 skips and one retained reference failure. All 75 nonpasses were reviewed with no unexplained results or skip differences. The 57 individual file-time outliers remain available for coupled measurements.

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

# Work queue

Workstreams found while closing conformance failures. Execute sequentially: the
owner limits this workspace to one task and one build or test workload at a time.
The current goal is functional/storage/client C++ parity with the lower-memory
performance acceptance in `docs/performance-acceptance.md`, not independent speed and
memory parity. The native-name integration now compiles across workspace targets;
historical full-corpus and shell results still require final-source revalidation.
Each names the C++ it ports and the tests or checks that close it. Finished streams are
removed; the commit history records them.

Priority: the shell (3) first — the owner's top priority (2026-10-21):
it must be DuckDB's shell one to one, interactive editing included — then compression
(5) and the rest; the C API leftovers (2) when a client needs them.

## Current verification queue (2026-11-02)

Current matrix update: all five declared Mac CP310–314functional cells and
independent reviews now pass (index1831:31,135matching XML records per engine,
392retained nonpasses,36typed WAL journeys,108coupled client samples). Current
iOS ARM build1686is the sole heavy job in1819. Earlier phase updates below are
historical; all other platform/client/storage/focused-control and final native
macro acceptance gates remain open.

Update2026-11-03local: CP313complete and independently reviewed; CP314strict build
is the only heavy job. Current process references remain in the live checkpoint.

The current signed Mac engine is core1637/source1634; corrected private Python
binding source1742 is separately verified. Native index1806covers the paired
SQL/cast/API/ADBC/Go/Swift/workspace/Mac-shell/storage gates. Mac Python3.10–3.14
functional cells and independent reviews1816/1822pass; CP313is complete and reviewed;
CP314strict build is running. All skips/failures and individual client timing outliers remain raw.

Current Apple SDK/full simulator/typed consumers, Linux current builds/signing,
all Linux runtime/client/storage gates and final130configuration TPC-H/ClickBench
coupled acceptance remain required. Native-Linux shell queue1825must execute the
399original Mac-blocked filenames and retain all10,865argv/path cases per target.

Focused native refresh1827is prepared and queued after1825. It reruns six unchanged
control payloads (HF/TLS, S3read/write, secret transactions/lifecycle/real commit
failure) against exact signed Mac C++934/Rust1637images. Every source/native hash,
pinned signature and sole-image check is required, and raw records are retained.
Only exact per-run fixture output paths and independently allocated loopback ports
may be substituted in comparisons. This is pending verification, not a newly
confirmed engine bug or a claim of whole HTTPFS/platform/release acceptance.
Historical controls below are component evidence, not current-artifact acceptance.

Live sessions and current receipts: `.context/corpus-resume-state.json`.
Use one heavy workload at low priority, jobs1, no subagents. Preserve the accepted
uncached memory policy and external-SSD evidence; required scope is unchanged.

## Historical blocking correctness checkpoint (2026-10-31)


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


**Core priority: complete the extension contract.** Control341 exposed the
missing C-ABI bridge in the Rust228 before-image. The live loader and local
installer now have the scoped checks above; finish network installation and
remaining metadata/version/nesting contracts. C++ binary ABI exclusions remain
separate. The completed
source226 paired SQL sweep is4,780PASS/75SKIP/14documentedFAIL/1sharedOOM/0timeouts
on Rust, with all fixture/signature/skip-reason checks stable342. Unstarted old
client/platform queues were deferred321/343; rebase after the core fix. No broader
completion or performance acceptance follows from these component results.


Temporary-directory creation, modes and errors must match C++. The nonrecursive
fix and confirmed Unix0755/OS-error/errno corrections are in frozen source226.
Signed227 passes21 unit plus11 integration tests under umask000, strict Clippy
and formatting. The signed before185 regression fails as expected. Public222
records the old engine's parent creation,0777 mode and incompatible error metadata.
Fresh signed native228 passes30 C API files/155 cases/924694 assertions. Public229
matches all five paired raw filesystem scenarios (ten executions) exactly; original fresh-root SQL230 passes
both engines and leaves missing parents absent. The signed235 Python wheel now passes the original238 and expanded239 Pandas3
controls. All four baseline/Pandas3 full-fast241/242 profiles also pass with exact
IDs/verdicts/skip reasons; all nine supplements244 also pass. Native ADBC247 passes64 cases/3040 assertions per engine, Go251 passes652 cases
per engine and consumer252 passes four typed WAL journeys. Swift254/255 and
manually reviewed shell258 also pass their scoped Linux x86_64 gates. Full paired
SQL260 has completed342; CP310263–266, Apple277 and simulator282 were deferred
for the confirmed C-ABI core fix. Other ABI/platform/storage/
performance gates remain. Then refresh full clients, the separate-root corpus, platforms,
storage/WAL and coupled performance. Preserve all old results; `dbgen_error`
is not a lower-memory waiver, and its SQL/ratchet must remain unchanged.

Pandas3 `str` is a confirmed client defect, not a source-audit suspicion. The
binder fix in source226 is verified on signed235 CP313 Linux x86_64: original238
eight cases match per Arrow19/21 profile, and expanded239 matches22+22+14 cases
including real-no-Arrow. Before177's four failures and all earlier controls remain preserved. Full241/242
baseline/Pandas3 matrices now pass on Arrow19/21, and all9supplements244 pass.
Finish the remaining ABI/target/client refreshes; native ADBC247 is verified. Pandas3
copy-on-write keeps the relation's original frame; preserve that observed C++
contract. Tensor/configured dependency coverage and historical reference-defect
classifications also remain open.

Use fresh candidate-specific Cargo targets for changing snapshot mounts: failed219
reused an old signed test executable despite current source hashes. Clean221/227
pass with genuinely new binaries; retain failed190/219 and superseded probe evidence.
External `platform-matrix-20261029/` receipts and `.context/current-verification-status.md`
record exact handles and lineage. Candidate200/201/203/207 were deferred, never passed.

## Public surface (M8)

2. **C API, the rest.** The integration checkpoint exports all 546 frozen duckdb.h
   functions plus `duckdb_adbc_init`. All 30 `test/api/capi` files pass at that snapshot
   (`scripts/capi-tests/run.sh`, as against the C++ library); final-source verification
   remains required.
   Left:
   - ADBC's 64-case frozen upstream suite passes on both engines with 3040 assertions;
     broader client/version/packaging matrices remain open. See `docs/adbc.md`.
   - Native producer projection/filter/dynamic verification is complete at source7:
     18 signed SDK C++ controls,16 public C stream/array comparisons,62 installed
     Python client controls per Arrow19/21 environment and both full fast matrices
     pass. Generic scans now deliver runtime filters and retain runtime bind data.
     A shared legacy public C API projection/filter limitation remains explicit.
     The693-file audit exposed two earlier compressed COPY memory regressions, now
     fixed by reusable Parquet dictionary metadata: original10/100MB tests, full693
     rerun,155 C API cases and12 cross-engine files pass with accounting intact.
     See docs/arrow-producer-parity.md and docs/parquet-dictionary-memory-parity.md.
   - Result Arrow settings snapshots are implemented and covered by the current signed
     30-file/155-case C API checkpoint; preserve that behavior in the final release gate.
   - Multi-statement PRAGMA extraction was a confirmed before-fix gap. A real
     one-table import reported four extracted statements on C++ versus one on Rust;
     chained/repeated contexts are 6/8 versus 3/2. Active-transaction imports also differ.
     The implemented `StatementPreprocessor` ports transaction wrapping/invalidation and
     actual import expansion; preserve raw file/name bytes and registered filesystem access,
     atomic rollback and statement-object/ADBC behavior. Do not merely adjust a count.
     Frozen paired C API counterexamples and identical export fixtures:
     `extension-overloads-20261026/import-extract-before-1-comparison.json`.
     A paired registered-memory filesystem import also succeeds on frozen C++ but
     fails opening schema.sql locally on the preserved signed main-context Rust wheel.
     This before-fix package control is separate from current catalog package acceptance:
     `import-preprocessing-20261027/registered-import-before-1-index.json`.
     The implementation now passes 15 signed component tests for actual expansion,
     implicit rollback, active-transaction invalidation, snapshot/reset lifetimes and
     registered byte-valued imports. All five public C API extraction contexts and seven prepare/execute journeys now
     match C++. Both fresh Python matrices pass, including 14 literal local/registered
     import journeys, two ADBC import controls and installed file handoffs each. The full
     C API suite passes 155 cases. Broader final gates remain; see
     `docs/statement-preprocessing-parity.md`.
17. **Complete language clients.** Owner-confirmed scope (2026-10-25): Python, Go
   and Swift only. Preserve existing C API and ADBC functionality; other language
   clients are outside this goal. Inventory the versioned API surfaces and packages,
   implement missing bindings/adapters, then run upstream suites and real installation,
   query, interchange, UDF and resource-lifetime journeys against the Rust engine.
   C API smoke tests alone do not establish client parity; bundling the C++ engine is
   not completion. Track evidence and remaining gates in `docs/language-bindings.md`.

3. **CLI shell, the engine's part** (top priority). `crates/shell` ports `tools/shell`
   one to one, linenoise included (`crates/shell/src/linenoise`, a module of the shell):
   the 392 tests of `tools/shell/tests` that pass on the C++ CLI pass on rustdb
   (`scripts/shell-tests.sh`), and interactive sessions (editing keys, history and its
   search, completion, highlighting, multi-line editing, the pager, `.edit`) match the
   C++ CLI byte for byte under a pty. `scripts/shell-diff.py` compares both binaries on
   the cases of `scripts/shell-diff/*.toml` (every mode and type, duckbox layouts, dot
   commands, options, errors, stream interleaving, Ctrl-C, pty sessions): all match but
   the ones marked `known`, listed below. Errors carry DuckDB's
   extra info (`error_subtype`, `position`, `name`, `type`, `candidates`, `catalog`,
   `schema`, `call`) for missing entries, existing entries, missing columns, unmatched
   functions and syntax errors, and `errors_as_json` lists it in DuckDB's order: libc++'s
   `unordered_map`, rebuilt each time DuckDB catches the exception into an `ErrorData`
   (`Error::with_extra_info_round_trip`, `Error::caught`). What still differs:
   - Internal errors have no `Stack Trace:` section. DuckDB's `Exception::ToJSON` adds
     the `backtrace()` addresses of an INTERNAL exception as `stack_trace_pointers`, and
     `ErrorData::ConstructFinalMessage` appends them symbolized (`StackTrace::
     ResolveStacktraceSymbols`): its own C++ frames, which rustdb cannot print. A
     section of rustdb's frames needs a backtrace captured in `Error::internal` and
     formatted like DuckDB's; its bytes would still differ.
   - The shell reads input and arguments as UTF-8, invalid bytes becoming U+FFFD;
     DuckDB passes the bytes on (a string constant then fails with "Invalid unicode
     (byte sequence mismatch) detected in value construction", `.print` writes them as
     they are). Needs byte strings from the shell's input down to the parser.
     Raw identifiers can also be created and queried in C++ while metadata conversion
     rejects them, so blanket validation of SQL input is incorrect. A shared checked
     byte-to-VARCHAR constructor is implemented for value construction;
     unchecked vector bytes and raw identifiers must remain distinct. Byte probes:
     `.context/artifacts/shell-bytes-20261023/`.
     That boundary passes 102 type/vector tests, all 30 C API files and 392 shell
     tests/three shared skips. A 65,823-input constructor matrix matches C++ exactly.
     Engine error messages and all native callback error carriers now own bytes:
     scalar, table, aggregate (including deferred errors), COPY, replacement and cast.
     SQL `error()`, native C API output and the shared cast/JSON transform/scanner path
     preserve bytes. NULs are sanitized before JSON conversion; JSON control escapes
     use uppercase hex. CAST retains source-column context; TRY_CAST retains the first
     nonempty error and NULL results. Aggregate state-size errors occur at physical
     preparation after optimization, including SQL PREPARE; pruned aggregates skip
     the callback and window aggregate size errors remain at execution. JSON-mode
     prepare no longer adds a caret to raw messages beginning with `{`.
     All 13,832 cases in `scripts/capi-error-bytes.py --family all` match C++, including
     all byte values/selected sequences, direct/prepared, plain/JSON, callback phases,
     deferred/empty-input errors, combine, pruning, SQL PREPARE, windows and recovery.
     The 35 function tests, workspace Clippy, 30 C API files, 765 shell cases (zero
     unexpected/five known), 392 shell tests/three skips and full 4,870-file corpus
     pass their gates. The corpus remains 4,781 pass, 15 accepted divergences and
     74 shared skips, with zero crashes/timeouts or ratchet regressions. Raw shell,
     parser/name bytes and legacy text wrappers are still open. Current artifacts:
     `.context/artifacts/callback-bytes-20261023/` (`first-path.txt`, `final-path.txt`);
     initial scalar/constructor evidence remains in `shell-bytes-20261023/`.
     Raw shell lines/history/SQL accumulation and dot-command tokenization now own
     bytes. Octal escapes retain invalid bytes; decoded NUL terminates one argument.
     Literal `.print` uses the byte callback interface, and raw command-name lookup
     and errors preserve bytes. The old `input-latin1-dot-print` known marker is removed.
     All 778 native byte comparisons match C++; 14 shell +23 common units, workspace
     Clippy, 768 shell cases (zero unexpected/four known) and 392 upstream shell tests
     with three shared skips pass. No full SQL sweep was repeated for this frontend
     step; the latest accepted full corpus is the callback-byte sweep above.
     Other metadata callbacks, invalid-byte error highlighting, argv and SQL/parser/name
     storage still have explicit text boundaries. No blanket UTF-8 SQL rejection was
     added. Probes/manifests: `.context/artifacts/shell-input-bytes-20261023/`.
     Its `raw-name-files/report.json` shows C++ can persist/reopen raw table, column,
     schema and type names; Rust rejects all four at UTF-8 string deserialization.
     Valid-UTF-8 controls match and all read-only hashes are unchanged. Continue with
     byte-owning typed identifiers across query/parser/catalog/serialization, using
     the binary deserializer's existing raw string-byte interface.
     Introduce an owned byte-string type with checked UTF-8 access (no implicit text
     dereference or lossy display), and a raw binary string codec before migrating
     generated AST/catalog name fields. Compare its numbered-property wire bytes
     against C++ for raw/empty/NUL/long strings. Keep strict String decoding for text
     fields; this codec foundation alone does not close the four raw-name file gaps.
     During the name migration, native formatting must also own bytes. Bound and
     parsed expression/query renderers must recursively use byte output rather than
     sending names through Rust Display. A byte-format helper may select a native
     renderer for byte-owning types and use ordinary Display for text/numeric types;
     ByteString itself must never gain a lossy Display or text dereference. Preserve
     numbered-property wire bytes, checked VARCHAR boundaries, keyword quoting and
     C++ formatting evidence while extending bindings and result names. Partial name
     migrations are not an accepted workspace or shell baseline.
   - Startup main/schema identities now match the frozen C++ values 20624/20622 across
     independent instances, and the shell database-list case matches literal bytes.
     Its obsolete known-difference waiver is removed. Later temporary-catalog,
     dependency-entry and registration-order identities remain an explicit audit; see
     `docs/catalog-oid-parity.md` and external
     `extension-overloads-20261026/public-catalog-comparison-1.json`.
   - A `memory_limit` far below an aggregate's state (`SELECT list(i) FROM range(1e7)`
     under `1MB`) fails in DuckDB and succeeds in rustdb (4).

## Storage

4. **Bulk-load memory, the rest.** Optimistic writes and concurrent checkpoints are
   ported, and materialized query results are buffer-managed like DuckDB's
   (`QueryResultMemoryType::BUFFER_MANAGED`, which the shell and the sqllogictest runner
   ask for; C API results stay on the heap, as DuckDB's `Connection::Query` keeps them):
   the result collectors append to a `ColumnDataCollection` (`crates/row/src/column_data`,
   DuckDB's segments, allocator and copy functions, nested types included, block for
   block), which spills or runs out of memory under a small `memory_limit` as DuckDB's does
   (`buffer_managed_query_result`) and which `pragma database_size` counts for the
   shell's last result. `PhysicalBatchInsert` throttles its unflushed batches with
   DuckDB's `BatchMemoryManager` (threads ahead of the lowest batch block while the
   batches take the memory it obtained; `MaxThreads` caps the threads by it). A commit
   that checkpoints and is estimated at `auto_checkpoint_skip_wal_threshold` bytes or
   more does not write its WAL entries: it takes the checkpoint lock before it commits,
   holds the WAL lock until the checkpoint ends, and fails if the checkpoint does
   (`skip_wal_write_due_to_checkpoint`). Left:
   - Ordered COPY reservation/accounting/throttling is implemented, including
     raw/prepared ownership, helper wakeups and lowest-batch forward progress.
     Preserve the rejected before/first-candidate evidence: the old unaccounted
     path was fast because it retained more raw data. The corrected source-8
     shipping calibration is 303.0 ms/459.5 MB versus C++ 246.6 ms/463.8 MB at
     14 threads/256 MB, compared with old Rust 87.4 ms/665.0 MB. The owner accepted
     this documented memory-bound tradeoff, conditional on the final TPC-H and
     ClickBench baseline. That historical calibration does not accept later
     release images. See `docs/batch-copy-memory-parity.md` and
     `docs/performance-acceptance.md`; final native coupled measurements and all
     query outliers remain required. In current source226/native228 full SQL260,
     unchanged csv_memory_management passes on both engines (Rust 119,032 ms,
     C++ 218,810 ms). These Rosetta whole-file timings have no coupled RSS and
     are correctness diagnostics, not macro performance acceptance.
5. **Compression parity, the rest.** ZSTD, roaring and hugeint bitpacking segments are
   DuckDB's (`zstd_compression_ratio`, `roaring_compression_ratio`,
   `roaring_bool_compression_ratio`, `bitpacking_compression_ratio_hugeint` pass;
   `crates/zstd` ports zstd 1.5.6's encoder and decoder, partial blocks are shared as
   `PartialBlockManager` shares them). Left:
   - All encoder strategy families, optimal splitting and automatic maximum-window
     LDM now have implementations. The original default-LDM failures pass unchanged;
     known70MiB,132MiB window-wrap, diverse and reused-context controls match C++.
     Signed debug/release each pass25 codec tests; workspace/all-target Clippy passes.
     Signed shipping integration passes20 cross-engine Parquet files, including
     70MiB automatic-LDM values; current FSST/native source3 retains the codec.
     Final release/query performance acceptance
     remains open; see `docs/zstd-strategy-parity.md` for exact scope/outliers.
   - FSST's controlled NUL-candidate ordering discrepancy is closed:73 exact
     primitive corpora/146 cross-decodes,124 signed affected tests per profile,
     190 affected SQL files and12 bidirectional storage/WAL/torn-tail mutation
     journeys pass. Historical5878/5877 density lacks a retained input and outer
     C++ sampling is random; do not treat it as a calibrated correctness gap.
     See `docs/fsst-parity.md`; final release/performance acceptance remains open.
   - zstd throughput is 0.88–1.0× DuckDB's (`docs/perf-backlog.md`).
6. **External file cache, the rest.** `ExternalFileCache` and `CachingFileSystem` are
   ported (`crates/function`: buffer-managed ranges, validation by version tag or
   modification time, `enable_external_file_cache`, `validate_external_file_cache`,
   `duckdb_external_file_cache()`), and the Parquet reader reads through them as
   DuckDB's does (the footer, prefetched row groups or columns, 256-byte page headers);
   the cache's ranges and memory match DuckDB's. Left:
   - `external_file_cache_eviction_queue` expects the scan of a table to evict the
     cached ranges: in DuckDB the materialized result (`COLUMN_DATA`) takes the memory.
     It passes once results are buffer-managed (4); a trial merge with that branch
     passes the whole directory.
   - In the prefetch mode, a read outside the prefetched ranges (page offsets outside
     their column chunk) is a direct read, where DuckDB prefetches up to 1 MB through
     the cache (`PREFETCH_FALLBACK_BUFFERSIZE`).
   - `CachingFileSystemWrapper` (the JSON reader, `read_text`, `read_blob`) caches
     remote files only: there is nothing to port until a remote file system exists.
15. **Parquet modular encryption supported path verified (2026-10-27).** The
    working writer/COPY FROM now supports footer-key AES-GCM encryption for all
    three key sizes, with module/footer framing and prepared-plan serialization.
    The reader now uses shared RustCrypto primitives rather than private AES.
    Signed C++/Rust cross-reading, installed Python local/registered-memory
    journeys and corruption controls pass. Both upstream encrypted-Parquet SQL
    tests also pass on both engines after replacing only their HTTPFS prerequisite
    with the explicit fallback-crypto test setting; encrypted SF1 export/import
    and all 22 TPCH answers through imported tables and Parquet views are covered.
    C++ also rejects column_keys, so that shared limit remains. See
    `docs/parquet-encryption-parity.md`. The broader final signed release/client/
    corpus/storage/performance gates still require refresh after this change.

16. **Persisted ART reload, remaining paths.** Modern disk allocator metadata is now
    restored for plain and stored constraint indexes without a table scan. ART buffers
    load lazily, keep unread disk blocks alive, hold the file pin while allocating and
    copying, and unload after checkpoint writes as C++ does. The profiling regression
    exposed by table caching passes again without changing counters or expectations.
    The broader full sweep caught a concurrent publication race: deferred index writes
    were flushed after releasing the index lock. Serialization now flushes within the
    bound-index guard or unbound-storage mutex, before exposing the new disk locations.
    Validation: 55 storage unit tests; 441 passes/ten shared skips over 451 affected
    SQL files; both concurrent failures pass three targeted trials and the cohort;
    ten C++/Rust file cases across storage 1.0/1.2/1.5 and GCM/CTR,
    including cross-engine mutation/reopen and PK/UNIQUE/FK checks. Every file case
    checks zero ART buffer residency before first access. C++ encrypted fixture writes
    use its documented `force_mbedtls_unsafe` test switch because this reference build
    has read-only crypto; C++ reads use normal crypto and fixtures contain synthetic data.
    Workspace Clippy, 30 C API files and 392 shell tests/three shared skips pass on
    fresh signed public binaries. The 763-case shell differential has no unexpected
    difference (five known shell engine gaps remain under item 3).
    A subsequent full sweep on `506354f` plus the cache flag has 4781 passes,
    15 accepted failures and 74 shared skips, zero crashes/timeouts and zero regressions
    across the unchanged 4855-file ratchet. This includes fixes for snapshot-tail
    filtering and partial mutation undo: 80 storage tests, 100 original stress trials,
    20 diagnostic trials and 374 affected SQL passes/one shared skip all pass. Forty
    C API stress trials and explicit-transaction lock behavior also pass against C++.
    Evidence:
    `.context/artifacts/index-reload-20261022/`.
    Native expression binding, buffered WAL mutations, native CREATE INDEX WAL buffers
    and pre-v0.10 root-pointer metadata are now verified in ART source14. Creating WAL
    transactions stage index publication until commit; automatic shutdown retains WAL
    while unbound expression keys await binding. Eight SDK tests per signed profile,
    138 broader tests, 796 SQL files (777 passes/19 exact C++ skips), 155 C API cases
    and 28 cross-engine recovery journeys pass. The cold 100k-row 1 MB no-spill
    checkpoint matches C++; the separate shared deleted-key reference limitation is
    explicit. See `docs/art-persistence-parity.md` and external
    `art-persistence-parity-20261029/`.
    Native ALTER ADD PRIMARY KEY WAL restore, streaming allocator buffers and
    constraint-order metadata matching are now verified in Resident source1. A
    resident-only old-layout checkpoint walk closes the two-million-row / 1 MB
    checkpoint failure without extra cache. The current full 4,870-file gate has
    4,781 passes/74 exact C++ skips/15 documented exceptions and zero regressions;
    C API 155 cases, 52 cross-engine journeys, 1,001 workspace tests and the complete
    shell gates pass. Paired recovery close time/RSS improves from about 82 ms/75 MiB
    to 18 ms/61–62 MiB; the remaining C++ gap is explicit. Missing/in-memory metadata
    retains the rebuild fallback. See `docs/art-resident-checkpoint-parity.md`.
    Final selected-client/ADBC/platform and TPC-H/ClickBench acceptance remains.

## Planner and execution

7. **Performance acceptance preserving the lower-memory baseline.** The owner
   confirmed on 2026-10-23 that roughly 15–20% longer execution with 60–70% less memory
   on TPC-H/ClickBench is acceptable. Preserve the uncached baseline; do not increase
   memory to copy C++ caching or close small speed gaps. The table-cache experiment is
   rejected, with its patch and evidence retained externally. Evaluate optimizations
   on both speed and memory, and report individual query outliers separately from
   averages. Current signed native acceptance is documented in `docs/performance-final-native-20261029.md`: all130 configurations pass and preserve the approved aggregate tradeoff; weighted sums, peak RSS and all63 individual outliers are explicit. Remaining Python/platform/core-contract acceptance is separate.
   Remaining useful optimization candidates include cheaper reads without higher
   residency, aggregate lookups/resizes and join probes, subject to that policy.
8. **Logical plan serialization, the rest.** `json_serialize_plan` writes logical plans
   with DuckDB's serializers: `tools/codegen` generates property structs from
   `logical_operator.json`, `expression.json`, `nodes.json`, `table_filter.json` and
   `column_index.json`, and the hand-written parts (`LogicalGet`, functions and their
   bind data, `COPY TO`, lambdas) follow DuckDB's. Functions carry their catalog entry,
   original arguments and a `serialize` callback; binds that replace the function
   object clear the entry, like DuckDB's. Parquet scans and writes, STRUCT `date_part`
   (ICU's too), binning and lambdas write their bind data; CSV and JSON scans and
   exported aggregate states fail as DuckDB's do. What still differs:
   - EXPORT serialization is closed at the retained component checkpoint:
     160 literal public plans,14 SDK cases and eight cross-engine export/import
     journeys pass. See `docs/export-plan-parity.md`; final current-image gates remain.
   - Aggregate callback/bind-data equality is closed at the current source5 component
     checkpoint:36 complete SDK plans/values/names per signed profile,50 native
     physical/serialized plan controls,324 affected SQL files and155 C API cases pass.
     Logical return types and eight aggregate bind-data families are compared like
     C++; C API simple-update capability selects the reference physical path.
     Three invalid C++ ordered-state array controls remain explicit reference defects;
     Rust defined sums and their exact plans are independently verified. See
     `docs/aggregate-equality-parity.md`; final release gates remain.
   - Encrypted-Parquet scan/COPY key serialization is now implemented. Current
     installed source8 Python controls on Arrow19/21 verify encrypted scan-plan
     serialization and paired C++ behavior; this earlier limitation is closed
     for the supported footer-key path. See `docs/parquet-encryption-parity.md`
     and `batch-copy-memory-20261027/python-encryption-arrow{19,21}-2/`.
   - Not comparable: `SELECT $1` and the optimized plans DuckDB fails to bind ("Failed
     to bind column reference ... inequal num bindings/types", e.g. TPC-H 2, 5, 7, 8, 9,
     12, 21 and `SUMMARIZE`) are DuckDB internal errors with stack addresses, and
     `now()` folds to the transaction's time.
9. **Physical operator logging, the rest.** Every `PhysicalOperatorLogType` site is
   ported (the hash join's `Finalize`, `Repartition` and `GetData`, `JoinHashTable`'s
   `Build`, the Parquet writer's `FlushRowGroup`, the reader's `ReadRowGroup` /
   `SkipRowGroup`), with the operator's type and `ParamsToString()`, and
   `debug_force_external` reaches the hash join, the sorts, the window merge sort tree
   and the temporary memory manager. Two `size`s differ: `Build` counts the build
   collection's chunk metadata as rustdb holds it (16 KiB where DuckDB's arena holds
   4 KiB), and `FlushRowGroup` estimates DuckDB's buffer (`ColumnDataCollection::
   SizeInBytes`: exact for fixed-size columns, strings and nested types approximate).
11. **ICU collation and sort keys — ported.** Merged as `2fa715c`. The runtime in
    `crates/icu/src/icu4c/collation*.rs` reads DuckDB's ICU 66 root data and tailorings
    from `crates/icu/data/icu66_collation.bin`; ICU4X is removed. The offline exporter
    (`scripts/icu-collation-data.sh`, `scripts/icu-collation-data.cpp`) uses DuckDB's
    embedded `icudt66l.dat`; no C++ runtime is shipped. Its upstream notice is retained
    in `crates/icu/LICENSE-ICU`. All 24 collation files pass, including
    `test_icu_extensive.test_slow` (`as`/`kok`/`mr`); five timezone files and plan
    serialization also pass. Fresh comparisons against C++ match all 130 collations'
    sort keys over 484,110 strings, plus locale keywords, names and BCP 47 tags.
    Evidence: `.context/artifacts/icu66-resume-20261022/`. The full sweep and two
    default-thread rechecks yield 4781 pass, 15 accepted divergences and 74 skips, with
    zero ratchet regressions. Rare ICU FCD iterator hangs intentionally terminate
    by collating the NFD form; these cannot be compared to a completed C++ key.
13. **Struct and VARIANT extract pushdown, the rest.** The pushdown is ported one to one:
    `RemoveUnusedColumns` registers `struct_extract` / `variant_extract` chains (under a
    cast) on scanned columns, scans read only the fields used (`ColumnIndex` children;
    the scan's `Projections` list them; Parquet reads only those fields), and a table
    scan of a STRUCT or VARIANT column emits each extracted field as a column of its own
    (`PUSHDOWN_EXTRACT`: `StructColumnData` reads the field into the result, with a
    pushed-down cast; `VariantColumnData::PushdownShreddedFieldExtract` reads a fully
    shredded field's typed column, else unshreds, extracts and casts). `variant/tpch_sf1`
    passes in 33 s (DuckDB's `unittest`: 89 s). DuckDB's quirks come along: a filter on
    a field disables the pushdown for the whole column; `FindShreddedColumnInternal`
    stops one path element short, so a single-field path (`v.a::INT`) never reads the
    typed column, and `v.a.b::INT` over a primitive `a` returns `a`'s values where the
    extract should be NULL (C++ 1.5.6 does the same); a typed and an untyped path of the
    same field in one query (`v.a.b::INT` with `v.a::INT`) raise DuckDB's "This path
    wasn't found in the registered paths" internal error. Left:
    - A VARIANT nested in a STRUCT has no column statistics of its own here (DuckDB asks
      its parent, `GetStatisticsRef`), so its shredded fields are never read directly:
      such scans unshred (same results).
    - Zone maps of pushed-down fields: a filter on such a column (only a join's dynamic
      filter can be one) does not prune row groups by the field's statistics
      (`ColumnData::CheckZonemap` with `PushdownExtract`).
    - Parquet reads only the needed struct fields when the file's column has the
      requested type; with schema evolution (a cast per file) it reads the column whole,
      where `MultiFileColumnMapper` maps the fields.
    - The nested/VARIANT hash combination gap is closed for defined values in the
      source4 component: 55 public C++ controls, 5,000 mixed rows and eight native
      cross-engine checkpoint/WAL journeys pass. The JSON converter also reserves
      the empty-container physical slot C++ uses. C++ raw hash instability from
      uninitialized NULL padding remains an explicit limited exception in
      `conformance/divergences.md`. See `docs/nested-hash-parity.md`; broader final
      artifact/corpus/client/performance verification still remains.
14. **Buffer eviction parity under tight limits, the rest.** `max_swap_space_error.test`
    passes (20 of 20 runs, alone and beside a directory run): an append that starts a new
    row group unpins the full row group's segments before it allocates the new ones, as
    DuckDB's `RowGroup::InitializeAppend` does, so a 1M-row insert under a 1 MiB limit
    spills until the swap limit ("failed to offload data block") instead of running out of
    pinnable memory. After `CREATE TABLE t2 AS SELECT random() FROM range(200000)` under
    that limit rustdb still keeps one 256 KiB block more resident than DuckDB (804,864
    against 542,720 bytes with a buffer-managed result, 1,067,008 against 804,864 with a
    streaming one): DuckDB charges its pipelines' chunks to the buffer pool
    (`BufferAllocator`, tag `ALLOCATOR`), rustdb's vectors are plain heap memory.

## Latest bounded accounting audit (2026-10-29)

The Resident native candidate freshly reproduces item14's one-block residency
exception: all eight one/14-thread,1/4MiB200k-row CTAS observations complete with
correct counts. Rust/C++ IN_MEMORY_TABLE at1MiB is1,067,008/804,864 bytes; at4MiB
both are2,377,728. Actual pipeline vectors still use untracked heap RawBuffer
allocations; do not erase the discrepancy by changing counters or limits.
Evidence: `final-verification-20261029/vector-accounting-1/`. Nested VARIANT parent
statistics, extracted-field zonemaps and schema-evolved Parquet projection remain
confirmed optimization differences needing observable impact/disposition; existing
fallbacks return the same values. The Python-only object-buffer candidate addresses
the separate release-wheel string-scan outlier and is not accepted before tests and
coupled measurements. Core native bytes remain unchanged by that candidate.
