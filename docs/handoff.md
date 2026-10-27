# Handoff

State of the DuckDB 1.5.6 → Rust port ("rustdb") on the main line
(`amirbaron/rust-duckdb-port`), updated 2026-10-25, to continue the work in a new session.

## Goal

Every test of DuckDB's sqllogictest corpus passes, except the ones rustdb cannot pass by
design (`conformance/divergences.md`: C++ extension binaries, and tests that expect an
out-of-memory error rustdb avoids by using less memory). Along the way: byte-compatible
storage and WAL with C++ DuckDB, the same C API, no C/C++ in the shipped artifact, and the
documented speed–memory acceptance of `docs/performance-acceptance.md`.

The owner updated the goal on 2026-10-23: preserve the measured lower-memory baseline;
roughly 15–20% longer execution with 60–70% less memory on TPC-H and ClickBench is
acceptable. Exact speed/memory parity and outperformance are not required. Do not raise
memory merely to copy C++ caching or close small speed gaps; report individual query
outliers. Functional, correctness and storage/WAL compatibility gates remain in place.
The owner narrowed the client scope on 2026-10-25: complete **Python, Go and Swift
only**, and preserve existing C API and ADBC functionality. Other language clients
are outside this goal. `docs/language-bindings.md` records the current scope.

## Latest encrypted-Parquet component stage (2026-10-27)

A completion audit and fresh C++/Rust probes exposed supported footer-key encrypted
Parquet writing/COPY FROM still unimplemented in Rust. The working implementation now
uses shared RustCrypto AES-GCM for 16/24/32-byte keys, matching module/footer framing and
configuration serialization. Three signed end-to-end SDK tests and 38 signed crypto/
Parquet unit tests pass, with two existing ignored Snappy tools. Workspace/all-target
check and Clippy pass. The complete overlay has 913 entries/912 build inputs.

Shipping images are signed; all 48 separate-process C++/Rust cross-read/COPY FROM
file cases pass (three key sizes, four codecs, both writers/readers). Shipping C API
retains 547 exports and zero test hooks. The signed Python wheel now passes 24 local/registered-memory encrypted-file
journeys in each Arrow 19/21 environment. Cross-engine reads and five invalid-input
controls per journey pass; all paired full error types/messages match C++ literally.
Both full fast Python matrices match C++ exactly (3,241/3,492 records). Both
upstream encrypted-Parquet SQL files also pass on each engine with the HTTPFS
prerequisite explicitly replaced for these supplemental runs; SF1 export/import
and all 22 answers through imported tables and Parquet views are covered. C API
is refreshing on the new shipping library. All prior
source-5 full corpus/client/performance checkpoints below remain preserved before-stage
controls and cannot substitute for final verification of these changes. See
[encrypted Parquet evidence](parquet-encryption-parity.md). The goal remains active.

## Next confirmed core gap (2026-10-27)

The encryption-stage C API refresh passes 30 files/155 cases/747,516 assertions.
A fresh public hash audit confirms all 30 VARIANT control expressions still differ
from C++. Frozen C++ recursively combines nested STRUCT fields and folds LIST/ARRAY
children into the existing seed; Rust currently reduces nested fields first and
combines once. Follow `docs/nested-hash-parity.md` and external
`nested-hash-parity-20261027/before-manifest.json`. The working nested-combine fix now passes workspace/all-target checking,
Clippy and all 25 signed row-layer unit tests. Its release SDK test is building
against 48 frozen C++ controls across constant/stored/selected and multi-argument
hashes. Source2 has 906 overlay files/905 inputs (Python bytecode caches excluded).
The first signed SDK execution exposed a JSON-empty-object hash counterexample;
its failure is preserved. Source3 builds a diagnostic SDK that reports every
mismatch. C++ reserves an offset-width slot even for empty containers; the JSON
converter omits it, unlike the ordinary typed VARIANT converter. See the nested
hash document for retained counterexamples. The full diagnostic isolates only
empty JSON objects among the initial48 controls. The JSON layout correction and
seven added stable C++ JSON controls are now in source4; Clippy passes and the
55-control signed release SDK now passes all 660 expected hash comparisons. Shipping
CLI/C API/SQL runner images are rebuilding from source4. Four C++ storage self-controls pass after
explicitly selecting required v1.5.0 storage. This component remains under
verification. Do not run redundant final-artifact gates on the encryption image before resolving
this confirmed core gap. Final performance acceptance still remains required.

## Signed nested-hash public/storage checkpoint (2026-10-27)

New signed shipping images preserve 547 C API exports/zero ADBC test hooks. All 55 public
hash/group/join controls and fully initialized 5,000 mixed rows match C++ literally.
All eight plain/encrypted bidirectional checkpoint/nonempty WAL journeys pass, including
hash indexes/generated columns/constraints/receiver mutation/original-writer readback.
The raw padded-VARIANT hash counterexample is C++ allocator instability, confirmed
by 423/5,000 differences between identical C++ processes; logical bytes/metadata and
all normalized hashes match. This is a narrow documented exception, not a broad
comparison mask. See `docs/nested-hash-parity.md` and external
`nested-hash-parity-20261027/{public-hashes-2/verification.json,storage-1/verification.json}`.
The current C API suite passes 30 files/155 cases/727,530 assertions with actual
loader-path/signature/source verification. The affected 760-file SQL cohort passes: 752 pass, seven exact C++ skips, one
existing avoided-OOM failure, with every verdict unchanged and complete fixture/
source/signature verification. The ordered COPY audit now confirms the listed missing manager: repeated
14-thread/256MB controls show Rust 666.1 MB median versus C++ 426.0 MB, with Rust
93.9 ms versus C++ 246.3 ms. Skewed Rust copies reach 905.3 MB; C++ returns OOM.
See `docs/batch-copy-memory-parity.md` for exact scope, safeguards and next
implementation. The first candidate now uses buffer-managed raw collections,
reservation-derived worker caps, raw/prepared accounting and streaming format
preparation. The first signed SDK passes error recovery and CSV, then hits its
180-second watchdog in pressure COPY; the missing worker-finish minimum-index/
wakeup handoff is now corrected. Source5 Clippy, all four signed SDK cases and
all 25 signed row-layer units pass (910 files/909 inputs). C++ controls pass all
four scenarios with COPY still at 96 MB and separate validation at 512 MB. Shipping
images are rebuilding for independent output and coupled before/after timing/RSS.
The first shipping measurement confirms lower memory but unacceptable runtime
regressions: regular14-thread/256MB saves33.9% versus old Rust for4.47times
the runtime;512MB saves8.8% for3.81times. All33successful files cross-read and
all payloads/order match. A profile and frozen C++ handoff review identify
unconditional local preparation after waking helper tasks; next restore the
C++ conditional handoff and retained PROCESSING_TASKS resume phase. The handoff
alone passes all four signed SDK cases (source6); the completed resume-state
source8 passes Clippy and all four signed SDK cases (0.66 seconds). The second
shipping build is signed and the36samplecohort completes with33successful
files independently cross-read for every payload/order. Regular14-thread256MB
newRust303.0ms459.5MB versus C++246.6ms463.8MB; oldRust87.4ms665.0MB.
New/C++ averagebusycores match1.69/1.72, while oldunaccountedpath uses5.98.
The512MB/skew costs remain explicit; user preference on documented COPY tradeoff
is pending. The current C API suite passes 30 files/155 cases/725,896 assertions;
both supplemental encrypted-Parquet SQL files pass on each engine. Expanded
1,059-file SQL coverage passes: 1,047 pass, 11 shared skips, one documented
avoided-OOM assertion; every verdict/failure payload is unchanged, with all
fixture/source/signature guards. The signed source8 Python wheel now matches both
complete Arrow fast matrices, all79non-fastrecords, installed ADBC/file/UDF
handoffs and all eight15-column cross-engine persistence/WAL journeys across
both environments. See `docs/python-bindings.md` for scope; configured-extension/
HTTPFS and broader API/platform audits remain. Go now passes all 652 pinned
root-suite cases exactly matching C++ IDs/verdicts on the signed library, with
79 upstream files/loader identity verified. Swift passes fresh 132+identity
cases on both engines, relocated/standalone distribution and four file/WAL
journeys. Native ADBC suites are running; test hooks remain isolated and the
feature-free shipping library was restored/verified before the harness.
Final acceptance remains open. Full final gates/performance remain open.

## Current integrated checkpoint and active full corpus (2026-10-27)

[Current-source verification](current-source-verification.md) is the consolidated index.
Source 5 has 908 verified build inputs. Current SDK15, C API155, ADBC64, Go652,
Swift132+identity, both complete Python fast matrices, full shell and the scalar/Swift/
15-column Python persistence-WAL cohorts pass their scoped gates. Native ADBC matches
C++64/3,040; its shipping build is restored with 547 exports and zero test hooks.
The full current 4,870-file SQL run is now accepted: 4,781 pass, 15 existing documented
failures, 74 exact C++ skips, no crashes/timeouts and zero ratchet regressions. Every verdict
is unchanged; only a generated worker-directory path differs in one failure payload,
verified by an exact two-occurrence mapping. Source/fixture/image identities verify.
Coupled performance measurement has started; acceptance remains pending.
Broader contract audits and explicit exceptions remain required. Do not mark the goal
complete from the completed component/client gates. External index:
`import-preprocessing-20261027/verification-checkpoint-2.json`.

## Latest import/preprocessing component checkpoint (2026-10-27)

The working implementation now expands actual import/PRAGMA statements with C++ transaction
wrapping and invalidation, and reads export files through local or registered filesystems
without losing path/name bytes. Runtime rollback, policy setting/reset and validated config
snapshot lifetimes are implemented. Workspace/all-target Clippy passes; all 15 signed SDK
cases pass (six import/preprocessing and nine registered storage), including raw-byte names
and implicit/explicit failure atomicity. Source 4 has 908 verified inputs in a complete
909-entry overlay. See [statement preprocessing evidence](statement-preprocessing-parity.md).

The second signed shipping refresh matches C++ in all five public extraction contexts
and all seven step-by-step prepare/execute journeys. Full C API passes 30 files/155 cases/
744,074 assertions. The fresh signed Python wheel matches all 14 local/registered-memory
import journeys and two ADBC import controls literally in Arrow 19/21; both complete fast
suites retain exact C++ IDs/verdicts/skip reasons (3,241/3,492 records). Both installed
Python/ADBC file handoffs pass with one Rust image each; all 908 source hashes verify.
Broader current-source regression and final release gates still remain. The
broader final corpus/shell/selected-client/ADBC/storage-WAL and coupled performance gates
remain open. No cache policy changed.

## Current next functional gap (2026-10-27)

The expanded signed current-core SQL cohort is verified: 785 exact files, 767 passes,
two existing failures, 16 exact C++ skips, no crashes/timeouts, and every verdict unchanged.
Source, images and complete SQL/data fixtures verify stable. Go 652/C API 155 and the four
native scalar checkpoint/WAL journeys below remain current for this source.

A fresh paired C API probe verifies an open import-extraction gap: one-table imports expose
four statements on C++ versus one on Rust; chained/repeated imports are 6/8 versus 3/2,
and active transactions also differ. Identical schema/load/CSV fixtures and native image
identities are preserved under `extension-overloads-20261026/import-extract-before-1-*`.
Port actual preprocessing, transaction wrappers/invalidation and import file expansion,
preserving byte-valued paths and checking registered-backend access and rollback. This
is next functional work; later numeric dependency/registration-order audits remain separate.
No final release/client/corpus/storage/performance completion is claimed.

## Latest catalog component checkpoint (2026-10-26)

The working tree now registers 214 typed extension overloads across 42 real scalar sets
before ordinary built-ins, with provider loading and delegation at bind time. Ordinary
attached catalogs expose their real provisional identity during initialization, followed
by once-only registered identity assignment. No numeric offsets or dummy objects were
introduced. All 18 signed catalog unit tests and three signed SDK tests pass; three
independent main databases expose the frozen C++ identity 20624, real date/list calls work,
and lazy ICU loading preserves timezone behavior. Workspace/all-target Clippy passes.

The new shipping images are signed and immutable. Fresh public probes confirm main
20624/schema20622 on three instances and all 2,953 function name/type rows. Go 652 cases,
C API 30 files/155 cases/743,370 assertions and four native scalar checkpoint/WAL journeys
pass on the selected image. The shell PRAGMA database_list case now matches literally;
its obsolete OID waiver is being removed. Other identities still differ, including the
temporary catalog and dependency-created entries. Prior full SQL/shell and Python/Swift
checkpoints below predate this stage.
Later dependency/merge lifecycles and final SQL/shell/selected-client/ADBC/storage-WAL/
coupled-performance gates remain open. See [catalog evidence](catalog-oid-parity.md) and
external `extension-overloads-20261026/{catalog-verification-1.json,sdk-verification-3.json}`.

## Latest main-context native checkpoint (2026-10-26)

Main startup now uses its actual connection, transaction and file-opener context, fixing
main-file disabled-filesystem commit rejection/rollback. Startup rejects nine runtime-only
settings as C++ does; opaque snapshots preserve validated allowlists separately from
instance manager state. Startup external-access restrictions allow only the explicit
main database and companion paths, preserving the C++ behavior.

The current signed build passes 35 SDK regressions, all 652 Go cases and the C API suite's
30 files/155 cases/704,678 assertions. Eleven startup and three database-policy controls,
plus the existing 49 startup controls, match C++. The targeted 589-file SQL cohort retains
every prior verdict: 571 pass, two existing failures, 16 exact C++ skips, no crashes/timeouts.
Four native checkpoint/nonempty-WAL journeys and 392 upstream shell tests pass (three
shared skips). All 903 source inputs and complete frozen SQL/data fixtures remain stable.
See [main-context evidence](catalog-oid-parity.md).

The full shell differential also passes all 768 cases, including 46 PTY sessions, with
zero unexpected differences and the same two documented differences. This is a scoped
checkpoint. The fresh signed Python wheel now matches the complete fast suites on both
Arrow 19 (3,241 records) and Arrow 21 (3,492), including case IDs/verdicts/skip reasons.
All 18 startup/policy controls match C++ in each environment and both installed Python/ADBC
file journeys pass with one Rust image. See [Python evidence](python-bindings.md).
Swift packages predate this stage. Final full-corpus/client/ADBC/storage/WAL and coupled
performance gates remain open;
remaining catalog identity/dependency lifecycles are not waived.

## Latest restart checkpoint (2026-10-26)

Configuration and client-session snapshots now preserve registered extension metadata,
global values, local overrides and search paths separately. Global RESET reaches
untouched/new connections, and NULL temp_directory exposes the disabled physical path.
Four signed SDK regressions pass; both exact frozen corpus regressions pass. The wider
421-file restart/settings/storage cohort retains every prior verdict. Current native
controls pass Go 652 cases, C API 30 files/155 cases/748,474 assertions, 49 startup cases,
and four bidirectional checkpoint/nonempty-WAL journeys. The 95 nullable-settings steps
retain 90 literal matches and five differences limited to native frame bodies.

Current shell verification passes 392 upstream tests with three shared skips, plus
768 differential cases/46 PTY with zero unexpected and two documented differences.
The new complete signed 4,870-file SQL corpus passes its scoped acceptance gate:
**4,781 pass, 15 existing documented failures, 74 exact shared skips, no crashes/timeouts**.
All verdicts match the prior accepted run, with zero ratchet regressions across 4,855
entries. Source, archived overlay, fixtures, reference and signed-runner identities verify
unchanged. Both earlier regressions pass. The pre-fix 4,779-pass run remains a preserved
counterexample, separate from this accepted checkpoint. See [restart evidence](catalog-oid-parity.md) and external
`shell-final-gaps-20261025/{restart-state-verification-1.json,full-corpus-restart-1/}`.

Python/Swift package results below predate this core checkpoint. Final selected-client,
ADBC, signed release, storage/WAL and coupled speed/memory acceptance remain open.
The remaining catalog identity and registered-filesystem database storage audits also
remain open. Historical checkpoint sections below retain their original scope.

## Current catalog startup work

Instance-owned OID allocation, lazy default entries and actual MVCC version-node IDs
are implemented in the working tree. Main database registration now follows storage
initialization; ordinary static extensions load after main attachment, with configured
extension options loaded when needed. The signed static-extension checkpoint passes
17 SDK tests, 652 Go cases and 155 C API cases across 30 files, plus workspace Clippy.

The next source stage separates the frozen 153 built-in and 367 core_functions
definitions, preserving source order and shared overloads. Its signed native image
passes all 652 Go cases, 155 C API cases and four bidirectional scalar-expression
checkpoint/WAL journeys. Workspace checking and Clippy pass. Expanded SDK persistence
and settings tests pass 17/17 on signed executables. This is staged evidence, not final full-source
acceptance: initial database OIDs are 20530 versus C++ 20624, with real placeholder,
collation and dependency entry lifecycles still to audit. See
[catalog evidence](catalog-oid-parity.md) and external
`shell-final-gaps-20261025/oid-core-registration-storage-1/summary.json`.

The complete 4870-file corpus checkpoint below predates these catalog changes.
Final signed SQL-corpus, shell, C API/ADBC, Python, Go, Swift, storage/WAL and coupled
speed/memory measurements remain open. Preserve the uncached lower-memory baseline
and report query outliers separately.

## Latest Go/core checkpoint

Pinned Go v2.10506.0 root suite with Arrow passes652/652on both Rust and frozen CPP,
with matching IDs/verdicts and the selected signed Rust image verified. Startup config,
access-mode readback, external-thread validation, prepared checkpoint execution/profiling/
pending errors, and comparison retry errors were corrected. Scoped public controls:
27startup,6thread,4checkpoint lifecycle cases match;547native exports remain available.
See `docs/go-bindings.md` and `.context/artifacts/go-client-20261025/verification-checkpoint-1.json`.
The signed SDK client-settings5/5and persistence10/10tests now pass; workspace all-target
Clippy passed. Current signed native image2also passes CAPI30files/155cases/749820assertions
and Go652/652again. Receipt: go-client-20261025/verification-checkpoint-3.json. CLI/Slt rebuild
completed and signed. Current shell passes392upstream tests/3same skips and full768-case
differential (46PTY) with0unexpected/2known open gaps. Fresh full4870corpus run5now passes the scoped gate:
4781pass/15same documented failures/74exactCPPskips,4855ratchet entries/0regressions, every
verdict unchanged and source/fixture/binary/archive/reference hashes stable. Evidence:
go-client-20261025/full-corpus-5/{terminal-summary.json,post-verification.json}. Earlier full
corpus4, Python6, shell11 and Swift verification predate these core changes and require
refresh. Python, Go, Swift, preserved C API/ADBC and final coupled performance acceptance
remain the goal; no full-table cache was added.

## Current Python and Swift package refresh

Python current signed wheel native SHAced8441f… matches both new Arrow19/21environments.
Both complete fast matrices match CPPexactly with zero failures (3241/3492records),336
Decimal outcomes,385exports,7stubs,strict typing and4current15columnfile/WAL journeys pass.
Receipt: python-core-refresh-20261025/verification-7.json. Swift current library2passes
132upstream+1identity case with exactCPPIDs, relative XCFramework/relocated/standalone
consumer and4cross-enginefile/WAL journeys. Receipt: swift-client-20261025/current-verification-2.json.
Broader client/backend/platform gates and final coupled performance acceptance remain open.

## Latest Swift package checkpoint

Relative Cduckdb XCFramework packaging and a separate relocated SwiftPM consumer now
pass on macOSarm64, with only the Rust framework loaded. A standalone copied executable/
framework also passes. Prepared/appender/rollback/file/result-ownership checks and all
four bidirectional checkpoint/nonempty-WAL handoffs with frozen C++ pass. Source/library/
package hashes and signatures remain verified; snapshots precede replay. Evidence index:
`.context/artifacts/swift-client-20261025/sdk-verification-1/index.json`.
See `docs/swift-bindings.md` for scope: broader API/platform/WAL and published SDK gates
remain open. No core cache or performance-policy change; final coupled timing/RSS and
individual-query acceptance remain pending.

## Ground rules (from the owner)

- **One task and one build or test workload at a time** (owner, 2026-10-22). Do not
  launch subagents or overlap builds, conformance runs and benchmarks. Use `-j 1` for
  Cargo, `--jobs 1` for test runners, and low process priority (`nice -n 10`).
- **Only one full corpus run at a time** on the machine
  (`ps -eo pid,etime,command | grep "slt run"`), and never rebuild `target/release/slt` in
  the same target directory while a full run uses it (the harness launches it per file).
- **Every built binary must be signed** with `scripts/sign.sh <binary>` (the rustdb-dev
  identity) before it runs, or it cannot access the external drive: `target/release/slt`,
  `target/release/rustdb`, `target/release/libduckdb.dylib`, test executables. Re-sign
  after every build.
- Big files on the external SSD: `/Volumes/amirs ssd/ai context/conductor/duckdb-rust/`
  (`targets/<name>`, `bench-data`, `logs/<name>`, `tmp`); `target` in this workspace is a
  symlink to `targets/ottawa`. Agents use `CARGO_TARGET_DIR=.../targets/<name>` plus
  `ln -sfn "$CARGO_TARGET_DIR" target` in their worktree.
- Subagents use model "opus". Commit messages end with the Co-Authored-By line; docs
  changes go in their own commit before the code.

## Swift current-host client checkpoint (2026-10-25)

Frozen Swift wrapper's complete132upstream cases pass on both CPPandRust, plus one
separate identity case with identical case IDs. In-process evidence confirms only the
selected signed Rust library is loaded. Dedicated test fixtures use a documented
path-only helper adaptation with all assertions unchanged. [Swift evidence](swift-bindings.md)
records preserved controls and limits: portable SDK distribution, broader client/API/
platform/storage journeys and other selected clients remain open. Whole goal is active.

## Fresh current-core corpus acceptance (2026-10-25)

Signed runner4 completed all4870frozen files:4781pass/15existing documented design
failures/74exact shared skips, no crashes/timeouts,4855ratchet entries with0regressions.
Runner/source/archive/reference/ratchet/fixture hashes remain unchanged. This is a
fresh complete current-core run, separate from the older corpus3snapshot. Evidence:
`scalar-release-20261025/full-corpus-4/terminal-summary.json` on the SSD and
[verification details](scalar-release-verification.md). Broader client/storage/WAL and
final coupled performance acceptance remain open. Swift controls/tooling are prepared;
actual Swift build/tests have not yet established parity.

## Current Python checkpoint (2026-10-25)

Signed wheel6 closes the three remaining fast-suite Decimal failures. Both Arrow19/21
complete fast suites match frozen CPPnodes/verdicts with zero failures;336focused
Decimal outcomes, runtime385exports, exact stubs and strict typing checks pass. Four
current15column checkpoint/nonempty WAL/receiver-write/writer-readback journeys pass.
See [Python Decimal verification](python-decimal-parity.md) and SSD
`shell-argv-20261025/python-validation-6.json`. Other documented reference/client/
environment/storage/performance gates remain open; this is not whole-port completion.

Core2CAPI30files/155cases/752694assertions and CLI11full768/46PTY/392upstream tests,
15+4signedunits/seven byte guards remain current for their unchanged compiled code.
Fresh corpus4now verifies current core; prior full-corpus3still denotes its older snapshot.

## Current core progress checkpoint (2026-10-25)

Core progress now preserves distinct unknown/started states, publishes0percent row
counters, accounts for the collector pull pipeline, and retains only scalar metadata
after partial close until the next preparation. Three signed SDK regressions and12
public C API lifecycle paths pass;547exports are unchanged and actual function images
are verified. [Progress evidence and limits](query-progress-parity.md) records raw
scheduler counts separately and the supported stored-table terminal fixture.

Signed CLI11 passes392upstream shell tests/3existing skips and retains all seven
native-byte guard probes' prior case/verdict sets. Six supported progress display /
cancel/reuse journeys pass fixed frame checks; timing-dependent full streams/counts
remain recorded separately. Full768case/46PTYdifferential is currently running.
`current-checkpoint-11.json` on the SSD indexes the source/image identities and gates.

Earlier CLI9/wheel4/full-corpus3 sections below describe prior core snapshots. Fresh
Python/native C API suite/core corpus/storageWAL/performance and all selected client
acceptance remain open. No table cache or fixed progress delay was introduced.

## Latest color/pager checkpoint (2026-10-25)

CLI7 closes all50measured color/pager byte mismatches:110/110default and2,915/2,915
exhaustive exact. Twelve real pager PTY journeys now match C++ (previously6/12),
including raw command/environment transport and return to stdout. Shell392pass/3skips,
15signed units and13affected differential cases/4PTYpass. The earlier full768/46PTY
result belongs to CLI6. Shared Python rendering source is unchanged from verified
wheel4. See [color/pager evidence](shell-ui-byte-parity.md) and SSD index
`shell-argv-20261025/ui-verification-1.json`. Remaining goal gates stay open.

## Latest shell/shared-renderer checkpoint (2026-10-25)

Signed CLI6 closes62of66settings-byte mismatches, with4raw Stack Trace differences
still open; exhaustive settings5,296/5,300exact. Shell392pass/3skips and full768case
/46PTYdifferential have no new differences; both known diagnostic/OID gaps remain.
Fresh signed Python wheel4 retains all prior fast-suite verdicts in Arrow19/21,
including three held Decimal failures. See [native shell evidence](shell-argv-parity.md)
and SSD indexes `shell-argv-20261025/{settings-verification-1,python-validation-4}.json`.
This is a scoped frontend/shared-renderer checkpoint; broader clients, current core,
storage/WAL and final coupled speed-memory acceptance remain open.

## Current release verification (2026-10-25)

Signed release3 completes the corrections found by the first new full corpus.
All seven affected files now pass on Rust and C++ through the corrected test adapter,
including the real disk-space requirement. Signed component tests pass126cases.
Workspace/all-target Clippy passes with warnings denied.

The exact preserved library passes all30 C API files (155cases /772,976assertions).
Runtime loader traces from every test executable prove the selected image; its SHA
stays unchanged. All547 shipping DuckDB exports are retained, without test bridges.
The shell passes392upstream tests /three identical skips; all395IDs/verdicts match C++.
Full differential: zero unexpected /768cases, including46PTY, with three known gaps
still OPEN (internal-error stack traces, database OIDs, invalid-UTF8 argv). The stale
stdin-Latin1 known marker is removed and its unchanged case passes strictly.

The fresh complete corpus on signed-images-3 is now TERMINAL: **4,781pass /15accepted
design failures /74sharedskips**, zero crashes/timeouts. Exact4,870-file coverage and
all74skip reasons match the frozen C++ reference. Ratchet:4,855listed /zero regressions
/zero newlypassing. All4,870test and1,522data hashes remain unchanged after the run.
Evidence: `.context/artifacts/scalar-release-20261025/release-verification-5.json` and
`full-corpus-3/terminal-summary.json`. This is a current-source SQL corpus gate, not
whole-port/client/storage/performance completion.

Native argv/state/rendering byte carriers are now implemented and measured. The
corrected shared renderer is verified in the signed Python wheel:32NULL/36width cases
match exactly; both fast dependency matrices retain exact previous node/verdict sets,
all385runtime exports and seven frozen stub files, plus installed strict typing checks.
Three held Decimal assertions and the string-columns reference discrepancy remain open.
The updated signed CLI passes392tests/three shared skips. Its expanded8,745caseprobe
has7,813exact matches,666trace-format mismatches and266hostfixture errors; raw mismatches
are not waived. Full768caseinteractive differential for the updated CLI is LIVE.
See `.context/artifacts/shell-argv-20261025/verification-checkpoint-4.json` and
[argv verification](shell-argv-parity.md). This source follows the corpus-accepted
snapshot; final current-core/version/WAL and broader client/performance gates remain.

The preceding full-corpus-1 remains4,774pass /21fail /75skip with seven ratchet
regressions; the corrected full-corpus-3 now passes the ratchet.
See [release checkpoint](scalar-release-verification.md). Broader Python and all
selected clients, storage/WAL matrices and documented final coupled speed-memory /
per-query acceptance remain OPEN. Preserve the lower-memory scan baseline.

## Current source integration (2026-10-24)

The checkout still has an **uncommitted native-byte migration**, but the **entire
workspace now compiles across all targets** (`capi-native-final-workspace-check-7`).
The check used one worker and keep-going; its terminal build-finished record is success.
Compilation is an integration gate, not final functionality or release acceptance.

The rebuilt Rust C API passes the existing upstream 30-file harness (155 test cases,
795,736 assertions; existing C++-internal test exclusions remain). All 8,735 native C ABI
outcomes match across query/result names, prepared parameter identities/execution,
STRUCT names, UNION names and aliases. A further seven-name C journey, including a real
C table callback, passes against both libraries. All 546 frozen duckdb.h functions are
exported, along with duckdb_adbc_init. The separate ENUM accessor
audit finds 1,613 matching returns and 134 C++-exception/Rust-return gaps. Do not claim
complete native C API correctness or ADBC/client completion.

C API/planner all-target Clippy with warnings denied and codegen pass; the prepared-name
panic fix has two signed release regression tests. Shell all-target Clippy and 15 signed
current-source tests pass at the preceding snapshot. The canonical shell differential
suite reports 0 unexpected/768 cases (three existing known) before the final width
correction; the corrected binary passes 154 native metadata cases. Two-way checkpoint
plus nonempty WAL replay passes for the recorded INTEGER/VARCHAR/NULL/Unicode scenario.
These remain scoped evidence, not full final shell/corpus/storage/WAL acceptance.

ADBC exports the frozen 1.0/1.1 initializer with all 54 assigned callbacks. The complete
64-case frozen upstream suite now passes on both engines with 3,040 assertions each.
The adapted suite retains every case/assertion and uses actual native APIs. Its Rust
run has no C++ DuckDB fallback; selected-image checks pass. Three opt-in test bridge
functions exist only in the separate test dylib. The restored/signed shipping library
still exposes 547 duckdb_ symbols and no test bridge.

The first Rust suite run passed 63/64: dynamic PIVOT's MULTI wrapper reached preparation.
Shared connection preprocessing now unpacks its enum creation + SELECT like C++.
Two signed runtime regressions (including invalid-UTF8 identifiers) and a maintained
public C probe on both shipping libraries pass. All 30 shipping C API files pass (155
cases/706,158 assertions this run); 13 signed C API unit tests pass. Default/test-feature
Clippy and workspace/all-target checking pass. Non-SELECT multi/pragma preprocessing
and transaction-policy extraction still need a broader core audit.

Earlier ADBC evidence includes seven public ABI families, cancellation/recovery,
12 real Python ADBC comparisons and 13 dense Arrow metadata comparisons; only verified
own build-version labels were normalized in those comparisons. Python ADBC validation
is not the DuckDB Python API/package. Required Python, Go and Swift clients, preserved C API/ADBC,
native/type/fuzz/package matrices, ENUM semantics and final unit/corpus/shell/storage/
WAL/performance gates remain required. See `docs/adbc.md` and external
`adbc-upstream-checkpoint.json`. The uncached lower-memory policy is unchanged.

The subsequent signed release C API checkpoint fixes result Arrow settings snapshots:
the public ABI journey matches C++ in21/21 cases versus12 old-library mismatches.
All30 upstream C API files pass155cases/778,832assertions; affected C++ controls and
warnings-denied Clippy pass. Failed-result options used for schema conversion expose
a separate unaccepted C++ INTERNAL/Rust-success discrepancy, retained explicitly.
Bounded coupled timing/RSS samples and both signed libraries are preserved under
capi-arrow-snapshot-20261025/. See `docs/capi-arrow-options.md`; this is scoped C API
evidence, not completion of other clients or final release acceptance.

Python binding work now has a pinned 1.5.6 source/API inventory (414 native surfaces)
and a real PyO3 package in crates/python + python/. The signed initial release wheel
installs and passes seven upstream tests. The current development wheel includes later
conversion/lifecycle corrections, native AST relations, bounded NumPy/Pandas output,
dataframe chunks, native Arrow interchange, registration/frame scans and direct native
Pandas/NumPy array scanning. Shallow dataframe copies preserve names and live-array
semantics; numeric chunks copy to vectors without whole-row snapshots or Arrow conversion.
External scan dependencies are copied with AST references and held by bound scans;
the lookup registry is weak. Native connection ownership is shared with transferred
readers, so they survive Python connection closure. It matches 49/51 Pandas, 30/30 reader, 32/32 input,
42/42 foundation, 30/30 relation, 36/36 joins/writes, 64/64 expression
and 75/76 host C++ outcomes. A fourth unaccepted case is string `render_mode="columns"`: C++
prints rows (its converter leaves a dangling pointer), while Rust prints columns.
Integer/enum column-mode rendering matches. One Arrow setting-change and two decimal cases are
unaccepted discrepancies: C++ emits empty text, drops a positive decimal exponent or
loses an infinity sign where Rust preserves the represented value. Raw evidence is
retained. Metadata binding preserves existing streams
and transactions, and relations retain their Python owner. The wider unmodified
101-test DB-API/cursor/type cohort now passes 101/101 on both engines with zero skips.
The full dataframe/join/query/create/insert journey passes. Native from_df/query_df
and NumPy input exist. The selected host cohort passes 37/37 on both engines, while
the broader object/analyzer cohort passes 79/81 versus C++81/81. Nested MAP promotion and typed STRUCT shape checks now pass; only the two decimal discrepancies remain in that cohort. The six-file Arrow cohort
passes 41/41 on both engines. Reader checks include cancellation/recovery, early release,
cross-connection scanning and an actual background thread. First-batch memory stays
bounded when declared row count grows from 1M to 100M; these development-wheel
measurements are not final release performance acceptance. A numeric input optimization
reduced the 1M-row scan from 85 ms to 6.8 ms at similar peak RSS; C++ took 3.3 ms.
Keep this individual development-wheel slowdown visible; do not use a suite average
or extra caching to declare performance acceptance. All nine
dataframe-chunk tests pass. The seven-file relation/join/expression cohort now collects
all 123 cases: Rust passes 123/skips 0, matching C++. The 50 formerly blocked
cases run. Native expression construction, immutable operators, cases/lambdas, column
attributes, type projection and EXPLAIN work; all 51 expression-only cases pass. Native/Arrow UDFs now pass all seven frozen
files: 559 passed, one existing xfail, with 26 focused C++ outcomes matching. Values/insertion, describe and update match 30, 14 and 23 focused C++ outcomes.
CSV read/write now pass 98 upstream cases plus one existing skip and 56 focused
outcomes (generated URI identifiers only normalized, raw errors retained). Native
snapshots use weak registered names and explicit relation ownership; native COPY writes
the original query. The CSV microbenchmarks retain a 1M-row slowdown and higher
whole-process peak despite lower file-like query increments; these are development
measurements, not release acceptance. Remaining gaps include other file formats/filesystems, closed-owner API coverage and argument-error details. All failures remain
recorded. No full Python parity or performance claim.
Relation show/str/repr share the native shell box renderer; 18/19 display outcomes
match, including cache/fetch independence. Nine signed catalog tests cover transactional
internal UDF removal and ordinary-drop protection. Parquet read/write now pass 57/57 primary cases; the broader four-file cohort is
59/60 on both engines with a shared timestamp-unit assertion failure. Fifty scoped
Parquet comparisons and 19 call diagnostics match (documented nondeterminism only
normalized, raw errors retained). Native function/cache tests pass 37/37 signed.
Type constructors/metadata and recursive target conversion now pass 34/34 upstream
cases and 84/84 focused outcomes. Python UNION conversion chooses the first exact or
convertible member, while SQL CAST keeps its ambiguity errors; five SQL controls match.
Two signed SDK tests verify native type-name lookup during streaming, failed lookups,
uncommitted enum types and rollback. Python/SDK Clippy and whole-workspace checks pass.
Default/explicit connection and catalog-name type coverage still need broader review.
Profiling APIs and auxiliary Python modules are now implemented/packaged. Two profiler
cases and 37 functional comparisons match; raw timing/resource metrics are retained.
Streaming EOF now flushes executor metrics before query termination; planner estimates
are included. Four signed SDK stream/type tests pass. Both packages collect 3212 fast
cases without collection errors; nine shared dependency/version file skips remain.
This is collection, not a full execution pass. All 3212 case node IDs match. Spark
runs 294 cases on both engines: 289 pass/4 existing skips/1 existing xfail. Native
read_json is implemented with all frozen options and native file-like/range/compression
support. JSON matches105/109 isolated outcomes; four overflow/file-like cases retain
unstable C++ callback errors and one abort, with safe Rust exceptions. This is an eighth
unaccepted discrepancy category. Eleven signed JSON tests pass, three timing probes
remain ignored. Bounded scratch reads reduce the 1M-row local JSON peak from about104MB
to59–60MB; local/prefix speed outliers remain visible. No table cache is added.
Previous JSON evidence is python-json-20261024/python-json-checkpoint.json; current
profiling/package evidence follows. Unnamed row STRUCT conversion is fixed;
18 typed row/NumPy/Pandas outcomes match without conflating container types.
Current profile/package evidence is
python-profile-20261024/python-profile-checkpoint.json and python-collection-20261024.
Polars now passes 58 cases with 2 existing version skips on both engines. Replacement
scans/dependency ownership pass 68/68 on both, closing 26 Rust failures. Relation-only
shared-binder capture retains external calls before CTE/scalar-subquery ASTs are
consumed. Sixteen scoped Python cohorts run 1681 cases:1668 pass/3 unchanged failures/
10 skip-or-xfail, not a full-suite pass. Focused Polars outcomes match 73/74; zero-batch
C++ memory runaway versus Rust bounded error is a ninth unaccepted discrepancy.
Same-connection lazy self-join concurrency remains unresolved (25/26 composed matches).
The 1M eager Polars conversion is a separate development outlier at roughly 2.2–2.6×
C++ elapsed time and 9–10 MB additional peak. No table cache added. Evidence is
python-polars-20261024/python-polars-checkpoint.json; earlier JSON/source controls remain.
A further explicit/default connection/result/value cohort now passes 126/126 on both
engines, closing 9 Rust failures. Module df overloads, aggregate/limit helpers, closed
Arrow results, BIT byte text, float/decimal rejection, NaN and datetime precision/
timezone conversion now match 56 focused outcomes; 84 type outcomes still match.
Seventeen development-wheel cohorts run 1807 cases: 1794 pass/3 unchanged failures/
10 skip-or-xfail, not a full-suite pass. Current Clippy/workspace checks pass.
Latest source/wheel evidence is python-runtime-20261025/python-runtime-checkpoint.json;
Polars performance/concurrency and all final acceptance gates remain open.
Native dataframe append and nonblocking closed-handle interrupt/progress checks now
pass the 41-case upstream threading/lifecycle/default-append cohort on both engines;
52 focused append outcomes match. The 18-cohort current run is 1848 total: 1835 pass/
3 unchanged failures/10 skip-or-xfail. Cross-engine Python append/checkpoint and
crash-left 999-byte WAL replay/read-write/readback pass in both directions over 15 columns,
including nested types, BIT and ENUM; pre-recovery files and hashes are retained.
The larger 100-repeat Polars audit finds 22 incorrect C++ results across 200 shared-context
runs, alongside varied closed-stream/pending errors; both engines pass all independent
and sequential controls. This is a tenth unaccepted category, not an accepted skip.
No stream-locking or table-cache workaround is added. Append 1M remains a separate
development speed outlier at 5.84–5.88 ms vs C++ 3.04–3.72 ms, with roughly 2–3 MB extra peak.
Coverage reconciliation leaves 1372 collected cases in 131 files outside current cohorts,
plus the missing-dependency/platform matrix. Latest evidence is
python-streams-20261025/python-streams-checkpoint.json. Final gates stay open.
Parsed statement objects, native discovery/load/install wrappers and connection-option
validation close six more failures: the complete connection API file now passes 48/48
on both engines; 131 focused outcomes match. Statement ASTs are retained natively;
batch execution prepares once and preserves last results/generator error ordering.
Eight signed SDK metadata/type/profile tests pass, including stream/rollback and
external-access checks. Eight bidirectional enum-pickle journeys over protocols 2–5
pass; C++ protocols 0/1 abort while Rust returns safely, an eleventh unaccepted category.
Current 19 cohorts run 1895 cases: 1882 pass/3 unchanged failures/10 skip-or-xfail. Clippy and
workspace/all-target checks pass. 1325 collected cases in 130 files remain outside these
cohorts, plus missing-dependency/platform matrices. Result-checked 1000-operation
microbenchmarks retain coupled time/RSS; they are not final release acceptance.
Latest evidence: python-connection-20261025/python-connection-checkpoint.json.
All-types 205/205 and timedelta/Arrow offsets 342/342 now pass on both engines. Native
Arrow producer projection/filtering closes five dictionary/pushdown failures; the
cohort matches C++ 155 pass/2 existing skips/2existing xfails. Four missing Arrow warning/
alias cases are fixed; Polars pushdown/refeed/deprecation pass 72/72. Correct fallback
column mapping, supported-filter gates, retained factory ownership and real producer
scanning are implemented; an excluded invalid-Parquet partition proves actual pruning.
Twenty focused producer journeys match. Current 23 cohorts run 2673 cases: 2656 pass/
3 unchanged failures/14 skip-or-xfail. 17 signed Arrow/SDK tests and Clippy/workspace checks
pass. 547 collected cases in 122 files remain, plus environment/version/platform gates.
Coupled Arrow microbenchmarks retain a slower first dataset sample and all RSS/time;
no result/table cache is added. Latest evidence:
python-all-types-20261025/python-all-types-checkpoint.json. Final gates remain open.
Native filesystem registration/streaming I/O and recursive VARIANT output close 40 more
failures. The protocol cohort matches C++ 65 pass/1 existing skip/3 existing xfails;
85 focused VARIANT outcomes match, filesystem 15/16 with a retained C++ temporary-URI
overwrite defect (twelfth unaccepted category). Native format reads/writes, globbing,
partitioned COPY and atomic positional callbacks are implemented without file snapshots.
Known-size uncompressed UTF-8 CSV buffer allocation is bounded by remaining file bytes,
removing a measured 28 MB small-file peak outlier; compressed/transcoded paths still pass.
Current 24 cohorts run 2742 cases: 2721 pass/3 unchanged failures/18 skip-or-xfail. 478 collected
cases in 117 files plus environment/platform/package/client gates remain. Latest evidence:
python-protocols-20261025/python-protocols-checkpoint.json. Arbitrary registered-backend
database storage and final core/shell/WAL/release/performance acceptance remain open.
Native GEOMETRY WKB host output and memoryview BLOB input close six scalar failures.
The complete scalar/SQLite cohort is C++ 79/79 and Rust 78/79, retaining the existing
positive-decimal-exponent discrepancy. Focused scalar journeys match 35/36: C++ ignores
memoryview strides and returns incorrect logical bytes; Rust honors them. This is the
thirteenth unaccepted category. Current 25 nonoverlapping cohorts run 2815 cases:
2793 pass/four held failure assertions/18 skip-or-xfail, with no collection errors.
The positive-exponent category is asserted in two cohorts. 405 collected cases in
104 files remain, plus environment/platform/package/client gates. Python Clippy and
workspace checks pass. Latest signed development wheel/source snapshot evidence:
python-scalars-20261025/python-scalars-checkpoint.json. Cache and performance policy
are unchanged; full final verification and documented performance acceptance remain open.
The remaining 31-file Pandas cohort now matches C++ 130 pass/one existing version skip,
closing 17 failures. Recursive NumPy object inference preserves MAP entries, typed
Value accepts type hints with C++ list/array inference, Arrow strings avoid deprecated
storage access, replacement errors keep their native classes, and empty relation ENUM
categories are retained while connection behavior stays distinct. New focused boundary
checks match 31/31. Direct UTF-8 chunk copies and singleton NULL checks reduce the
one-million-row string-scan time by about 52%/40% without materially raising memory.
The remaining 1.9×/2.0× slowdown (Rust dev versus C++ release) stays an individual open
outlier; see performance-acceptance.md. Current 26 cohorts run 2946 cases: 2923 pass,
four held failure assertions, 19 skip/xfail. 274 collected cases in 73 files remain.
Latest evidence: python-pandas-remaining-20261025/python-pandas-remaining-checkpoint.json.
The next Arrow/API cohorts close 13 Rust-only failures, including a keyboard-interrupt
timeout. Arrow matches 77 pass/two existing skips. API matches 86 pass/one shared
UTC-assuming timezone assertion/one existing xfail; explicit UTC controls pass 87/one
xfail on both engines. Native pending tasks poll Python signals and preserve cleanup;
all four warmed cancellation/reuse journeys match. BIGNUM text output, zero-batch reader
ownership/errors, indexing, joins, tuple conversion, missing ExplainType, progress and
startup secret options are corrected. Explain/statement checks match 175/175 and reader
boundaries 92/92. Ten signed SDK tests pass. Final signed development wheel runs 3113
cases: 3086 pass/five held assertions/22 skip-or-xfail, zero collection errors; 107
collected cases in 48 files remain, plus environment/platform/client/final gates.
Python/SDK Clippy and workspace checks pass. Coupled first-batch reads stay bounded at
about 73 MB for 1M/100M sources; the 0.1–0.2 ms latency gap and noisier earlier insert
measurements remain visible. No caching policy changes. Full persistent-secret format
compatibility is not proved by the isolated disabled-read fixture. Latest evidence:
python-arrow-remaining-20261025/python-arrow-remaining-checkpoint.json.
The remaining 48-file fast cohort now matches C++ 100 pass/seven existing skips,
closing five failures in NumPy arrays, Arrow protocol diagnostics and typed JSON errors.
Full unmodified fast-suite runs with pinned dependencies and TZ=UTC now have 3221
JUnit records: C++ 3183 pass/38 skip-or-xfail; final signed Rust development wheel
3180 pass/three held Decimal assertions/38 skip-or-xfail, no collection errors. Passed
counts include one XPASS. All 3212 original collected test IDs are matched to execution
records on both engines; the additional nine records are skipped dependency/version
modules. Seven non-fast files and those dependency/platform matrices remain open.
The package supports direct `_duckdb` import first and adds 49 low-level helper exports;
fresh-process default-owner/query/cursor/metadata/reopen journeys match. Runtime API
audit resolves stub aliases: 362/385 C++ runtime entries exist, 23 remain missing; 29
stub declarations are typing-only. Pinned native `.pyi` files are not yet shipped.
Focused probes are 30/31 raw exact/all31 parsed semantic outcomes, retaining one Binder
JSON key-order gap. A rejected rethrow attempt affected candidate errors; planner SHA
is restored exactly to the preceding source. Python Clippy/workspace checks pass and
cache/performance policy is unchanged. Latest evidence:
python-tail-20261025/python-tail-checkpoint.json. This is not Python/client completion.
The 23 runtime availability gaps are now closed: all385 frozen runtime inventory entries
exist. Shared native type constructors/default and per-connection catalog lookup match
225/225; native byte tokenizer/token enum 298/298; explicit getters/legacy division/
destructor/cursor/reader lifetime 13/13. Separate CPU environments verify actual PyTorch
and TensorFlow conversions 51/51 each; unmodified upstream TF passes, upstream Torch
keeps its existing skip. Original control dependencies are unchanged.
Active queries retain a read-only callback catalog context, released with query end;
12 signed SDK tests pass, including 10001-row streaming lookup and raw-byte type names.
C++ same-connection UDF dtype lookup hangs in an isolated eight-second control; Rust
resolves safely (fourteenth unaccepted reference discrepancy, not accepted/emulated).
An eager-default initialization caused an initial ~4MB increase and was removed; final
before/after runs have similar bounded RSS. Insert-many is ~6% longer, parsed/text similar;
all rounds/results retained, no cache added. Whole fast suite again3180pass/threeheld
Decimalassertions/38skip-xfail, all3212collectedIDs reconciled. Clippy Python/SDK/parser/CSV
and workspace checks pass. Signatures/overloads/lifetimes, stubs, remaining dependency/
platform and seven non-fast files, other clients and final core/shell/storage/WAL/release/
performance acceptance remain open. Latest evidence:
python-type-api-20261025/python-type-api-checkpoint.json.
The following non-fast/default-fixture checkpoint invokes all seven upstream files:
both engines record 71 passes/eight skips across 79 records, including all30 H2O Arrow
cases and the 10M-row materialized fetch. One extension-loading pass is a fixture-unset
no-op; configured extension loading and HTTPFS remain open. All six pinned native
stub files now ship unchanged in a signed wheel, plus a relative-extension forwarding
stub. Installed strict mypy positive clients pass and negative controls reject four
errors each, with provenance confirming installed paths. Runtime code is unchanged
from the preceding Python checkpoint. Evidence: python-nonfast-20261025/, especially
nonfast-comparison.json and wheel-stubs-verified.json. Broader signature/runtime,
dependency/platform, other-client and final acceptance gates remain open.
The following Python wheel now includes the frozen ADBC facade and native initializer
in its own Rust image (547 shipping symbols, no test bridge). All 23 previously skipped
upstream ADBC tests pass on both engines. Sequential file/transaction/UDF-write/reopen
and one-image installed journeys match; live shared-instance access is not asserted
(the initial C++ harness expectation failed). Full fast runs now have 3241 records:
C++ 3206 pass/35 skip-xfail; Rust 3203 pass/the same three held Decimal assertions/
35 skip-xfail, zero collection errors. All 3235 normal cases reconcile: previous
3212 plus 23 ADBC.
Initial incomplete dependency setup errors and corrected final runs are retained.
31 facade declarations resolve on both packages; broader semantics remain partial.
Coupled before/after statement/reader measurements stay bounded; no cache added.
Python/CAPI Clippy and workspace/all-target checks pass. Evidence:
python-adbc-package-20261025/ and `docs/python-adbc-inventory.json`. Other client and
final Python/core/shell/storage/WAL/release/performance gates remain open.
The subsequent PyArrow 21.0.0 matrix keeps the 19.0.1 baseline environments intact.
Both Arrow cohorts pass 886 cases/14 skips/two xfails; run-end coverage is 250 passes/
two existing BIT/UUID skips. Full fast reruns have 3492 records: C++ 3456 pass/36
skip-xfail; Rust 3453 pass/the same three held Decimal assertions/36 skip-xfail,
zero collection errors. All 3487 normal cases reconcile, including 252 newly enabled
run-end IDs. The Parquet7652 module still skips because parquet has no version
attribute; its unchanged class/method AST passes separately on both real engines,
explicitly an adapted journey rather than a pytest pass. First-batch RSS stays bounded
for 1M/100M rows; the 0.45–0.52 ms Rust versus 0.26–0.39 ms C++ latency remains visible.
No engine code or cache change; same signed ADBC wheel. Evidence:
python-arrow21-20261025/. Other dependency/platform and final client/core acceptance
gates remain open.
The next Python helper checkpoint fixes invalid connections being treated as omitted
and valid false-valued connections being replaced by the default. It uses explicit
None selection and frozen diagnostics across 68 module helpers, without changing
runtime algorithms. The 396-case provided-argument cohort now matches C++ completely
(previously zero); the 1872-case boundary cohort and 28-case state/frame/exception
journey also match. These cohorts overlap; do not sum their counts. The final signed
wheel 3 again runs both whole fast matrices: Arrow19 3203 pass/three held/35 skip-xfail;
Arrow21 3453 pass/the same three held/36 skip-xfail, every C++ node covered and no
collection errors. Installed typing positive/negative checks and 385-export audit pass.
Module-helper timings remain around 74–77 ms/1000 calls at ordinary 46–48 MB peaks;
reader prefixes stay bounded around 73 MB. No eager default DB or table cache added.
Evidence: python-root-contract-20261025/. Argument-value conversions and broader
semantics remain partial, alongside the existing client/core/final acceptance gates.
The following byte-alias checkpoint accepts str/bytes/bytearray for connection
sql/query/from_query and relation set_alias, preserving NUL/invalid UTF-8 bytes.
It corrects from_query to keyword-only alias/empty default. The alias matrix improves
20/2670 to 2670/2670; 94 state outcomes, 432 expanded connection cases, 1872 helper
boundaries and 225 constructor outcomes match. Component cohorts overlap; generated
C++ aliases are compared by shape/uniqueness with actual random values retained.
Failed repr diagnostics use C++'s fallback; existing rendered strings remain available
after close, with GIL-free waiting. Actual-type connection checks reject spoofed
__class__. Concurrent render output matches without deadlock, but C++ observes two
callbacks versus one on both previous/current Rust: this discrepancy is retained.
Final signed wheel 4 keeps the full fast counts on both Arrow versions unchanged,
with every C++ node covered and no collection errors; typing/export audits, Python
Clippy and workspace checks pass. Query/alias micro timings remain about 23–26 ms/1000
at ordinary 45–48 MB peaks, reader prefixes around 73 MB, cold samples retained.
Evidence: python-name-contract-20261025/. Table/view and other string boundaries,
broader Python semantics and all existing final/client/core gates remain open.
The next catalog-name checkpoint ports register/unregister/table/view to native byte
names, including direct view AST names and C++'s distinct physical-table versus view
fallback. A new SDK metadata lookup preserves active streams/transactions. Expanded
catalog outcomes match 3993/3993; 18 state checks and two signed SDK tests pass.
Empty-view names return a checked C++-compatible InternalException without assertions.
Decoder exception arguments survive the engine's owned error channel.
Harness correction: old broad hex normalization also masked decoder byte values.
New receipts normalize only actual object addresses; raw old receipts remain, and
strict fresh alias2670/state94 comparisons still match. Full fast counts on both
Arrow versions remain unchanged with every C++ node covered and no collection errors.
Typing/export audits, Python/SDK Clippy and workspace checking pass. Lookup microcosts
remain visible: physical 8.5–8.9 to10.9–11.2 ms/1000; registered-table22–23 to27–28 ms;
direct-view/registration similar, ordinary118–119 MB RSS, prefixes~73 MB. No cache
added. Evidence: python-catalog-names-20261025/, final signed wheel3. Other binding/
core/final acceptance gates remain open.
The following table-function checkpoint fixes byte casting and literal dotted names;
the name/argument matrix improves 35/842 to 842/842. Python scan names/signatures now
match C++ (arrow_scan/arrow_scan_dumb three POINTERs, pandas_scan one POINTER), removing
private scan names from suggestions. Native Arrow producers delegate to the original
bind path; Python producers retain owners. One signed SDK test proves adapter
replacement preserves a running 10001-row stream and affects later binds. A real Arrow
journey matches catalog/output and source lifetime across another connection opening.
Deferred PyO3 decrefs previously retained input until another native call; final EOF/
release cleanup fixes this after dropping locks and avoids shutdown/GC attachment.
Shutdown checks pass. Final signed wheel 3 has unchanged whole fast counts on Arrow 19/
21, no new failures/collection errors and all C++ nodes covered. Typing/export/compiler
checks pass. Table-function 1000 calls 37.0–37.5 ms before/37.7–38.4 after, ordinary
46–48 MB; Arrow scans and bounded 73 MB prefix samples retained. Evidence:
python-function-names-20261025/. Scalar UDF and other string boundaries, other clients
and final core acceptance remain open.
The subsequent scalar-UDF checkpoint migrates ScalarFunction/Set names to native
bytes throughout catalog/binder/display/optimizer/serialization and ports Python
create/remove/FunctionExpression casters. The UDF matrix improves15/804to804/804,
with eight further name/ownership/closed-state matches. Both complete fast matrices
have unchanged counts/no new failures/collection errors/all C++ IDs covered. Workspace
all-target Clippy-Dwarnings passes; seven signed library binaries pass105tests and
two signed raw/NUL scalar regressions pass. Focused cross-engine checkpoint/WAL
checks of generated/default/constraint/index scalar expressions pass in both directions;
the fixture WALs are identical156bytes. Broader storage acceptance remains open.
Scalar/group/UDF paired time/RSS remains similar, cold samples retained, no cache added.
Evidence: python-udf-names-20261025/. Current fresh release CLI/CAPI/slt build is underway
under scalar-release-20261025/; do not treat its start as terminal verification. Broader
core/corpus/shell and selected-client/final performance gates remain open.
Fresh scalar release build 1 completes and its shell/CAPI/slt images are signed and
SHA-preserved under scalar-release-20261025/signed-images/. The rebuilt C API passes
all 30 upstream harness files:155 cases/727632 assertions. Shell pytest required a
harness option-order correction and an SSD lexical tmp_path alias: nine failures also
occurred on C++ because upstream dot commands omit quotes around temp paths. The
fixture adapter delegates original pytest cleanup and changes only the returned path;
test bodies/assertions remain unchanged. C++ then passes392/3skips; Rust391/one real
failure/3skips. Investigation finds FIFO fstat size can describe queued bytes, not
total CSV length. CSV native file capability now uses metadata.is_file() rather than
unconditionally treating a FIFO as seekable. One signed FIFO buffer regression passes,
and the signed corrected DEVELOPMENT shell matches392/3skips. CSV all-target Clippy
passes. Release build 2 with the pipe correction is now active, not yet acceptance;
after terminal/signing rerun release shell/CAPI/differential then one fresh fullcorpus.
Do not rebuild Slt while its corpus is running; preserve earlier images/results.
Previous nested/type evidence: python-nested-20261024/python-nested-checkpoint.json, signed wheel9,
source archive, complete scoped regressions and repeated coupled UDF timings. The
native UDF slowdown remains an individual outlier; a noisy C++ run is retained.
The complete closed-owner relation case now passes after native dataframe-map support
and set-operation owner validation. Map passes 11/11 upstream cases and matches 82/84
focused outcomes; two reference lifetime defects remain unaccepted (unstable numeric
sums and a mapped-view callback crash). Bounded native numeric conversion reduces
1M-row identity mapping from about 60 ms to about 42.5 ms at about 117 MB peak RSS,
versus C++ about 38–39 ms/127 MB. Both engines retain only the latest output frame
for checked measurements; the original failing reference journeys remain preserved.
The source/archive manifest and raw evidence are under python-map-20261024. Remaining
relation APIs, callback persistence/platform behavior and other client gates stay open. Aggregate/window convenience families now pass 108 upstream cases with one existing
skip; 79/81 focused outcomes match. A fifth unaccepted discrepancy is C++
RTrim removing trailing UTF-8 bytes and sometimes aggregating the wrong column.
Rust preserves the identifier. Grouping/window microbenchmarks retain individual
slowdowns and lower peaks; direct SQL reproduces them. Root identity checks show the Rust extension as the only loaded DuckDB engine. Native
Clippy and workspace/all-target checking pass. Full relation/expression APIs, remaining UDF overload/annotation/concurrency coverage,
full native Pandas/NumPy semantics, frame/protocol/pushdown and remaining interchange and
type/platform/package tests remain open. See docs/python-bindings.md and external
python-aggregates-20261024/python-aggregates-checkpoint.json. The foundation checkpoint remains historical; an
accidental collision in its run 3/4 JSON filenames is documented in the relation
evidence manifest, with new reports separated and original comparison-4 data restored.
Use the external CARGO_HOME/PYO3_PYTHON=Python3.13
for subsequent Python/workspace builds; keep builds/tests serial and sign wheels.

Historical corpus and benchmark figures below describe previously accepted binaries.
The new migration still needs the full unit/corpus/shell/storage/WAL/client verification
and documented speed-memory acceptance. Strict PEG raw ingress, remaining text/value
codecs and fallible-rendering boundaries are open. Python and every selected language
client remain required; preserve the uncached lower-memory controls. The following
migration notes are a chronological checkpoint journal; old diagnostic counts and
pending statuses describe those snapshots and are superseded by the current state above.

New focused evidence includes 5,241 C++ index descriptor cases, 6,988 CTE/macro/lambda
codec cases, 3,494 native exception metadata cases, and 8,050 parser AST/rendering
comparisons plus 26 acceptance/rejection decisions. The 700 invalid-UTF8 parser inputs
remain pending raw ingress and are not counted as passed. Lambda-reference codec/name
checks do not establish SQL rendering parity: C++ deliberately rejects rendering that
transient node, while the current Rust renderer returns its name. Record and close
that difference before final acceptance.

The latest table/pivot stage passes all 218 affected release tests (one pre-existing
exhaustive cast-grid test remains ignored), 15,723 new C++ pivot/value comparisons and
the previous 8,735 cast comparisons. The 15,723 cases cover 3,494 pivot/unpivot AST
wire/render cases and 12,229 value/cast cases; the latter compare successes and exact
exceptions, including 841 plain/cast and 561 SQL exception outcomes. C++ validates
VARCHAR when materializing values: nested values with invalid name bytes can fail
`ToString`, even though a STRUCT SQL literal can keep those bytes.

`Value::to_varchar_bytes` and `try_to_sql_string` expose that native behavior. The
infallible `to_sql_string` wrapper and old text callers still need migration so rendering
exceptions reach the engine/shell; this is not complete native rendering parity. An
unnamed-STRUCT-to-VARIANT probe also fixed a real cast-error distinction: C++ exceptions
from cast binding/execution escape unchanged; only recorded conversion failures get
"Failed to cast value". Scoped common/types/function/AST/parser Clippy with warnings
denied and codegen --check pass. Final workspace compilation and runtime gates remain
open. Exact logs and hashes: `table-name-checkpoint.json` in the external artifact folder.

The statement-name stage now carries INSERT target qualifiers/columns, UPDATE SET
columns, MERGE action/source names, JOIN USING names and COPY column metadata as bytes.
It passes all 86 affected function/AST/parser release tests (one pre-existing exhaustive
cast-grid test remains ignored), 5,241 new C++ comparisons and the prior parser oracle.
The new cases comprise 1,747 INSERT SQL renderings, 1,747 JOIN USING wire/restored-SQL
cases and 1,747 COPY ParseInfo wire/name cases. COPY cases do not verify COPY SQL text or
file execution. Scoped Clippy with warnings denied and codegen --check pass.

The workspace still does not build. Remaining statement and table-name errors reach
real catalog/search-path APIs and filesystem boundaries that must accept native bytes;
do not decode names to text merely to make compilation succeed. No full engine or
client/runtime acceptance is established by these focused tests. Evidence and source
hashes are in `statement-name-checkpoint.json` under the external artifact folder.

The native catalog/storage stage now passes **287 core release tests plus 23 parser
release tests** (one pre-existing exhaustive cast-grid test remains ignored), scoped
all-target Clippy and codegen checks. Catalog names/keys, object/dependency/view names,
type aliases, table storage names and catalog undo/WAL/checkpoint carriers retain bytes.
Snapshot rollback/rename tests cover all 256 byte values, and Unicode names are kept
distinct under C++'s ASCII-only identifier folding.

The four preserved C++ raw-name databases (table, column, schema and enum-type names)
and their four UTF-8 controls now load and scan through the signed Rust catalog/storage
SDK. Copies were appended to, checkpointed by Rust, reopened by Rust and queried by the
C++ CLI: **all eight cross-engine round trips pass**, with two expected rows each. All
original hashes are unchanged. A native C++ metadata probe confirms that the stored enum
column has no alias; an initial harness assumption expecting one was corrected. The
reader is a Rust catalog example, not the public Rust shell: full Rust SQL/shell and WAL
recovery remain unverified. Round-trip report pointer:
`identifier-bytes-20261023/catalog-file-roundtrip-path.txt` under the external artifact
root. No C++ DuckDB engine is linked into the Rust reader.

Context search paths/lookups, CREATE/ALTER and table metadata now retain bytes through
their planner interfaces. Transaction database names and sequence WAL records use byte
carriers and explicit native string serialization. Prepared-parameter maps, export
metadata, COLUMNS aliases and debug plan rendering have also been integrated. Export
filename sanitization now replaces each non-letter byte, matching the frozen C++ source.
That checkpoint had seven planner diagnostics; the renderer continuation below closes
those library compilation errors. Downstream execution/result consumers have not yet
been checked by a successful full build. Rebuild the entire engine and repeat real SQL, shell, corpus
and WAL journeys before claiming acceptance. The logger interface now has a native byte-message method; the runtime logger
implementation and native plan/value formatting callers still require integration.
These remaining gates, language clients and final performance acceptance stay open.

The Context/binder continuation passes **310 core/parser release tests**, with the same
one pre-existing exhaustive cast-grid test ignored. All affected core packages pass
all-target Clippy with warnings denied; codegen --check passes. These tests do not build
or execute the planner. Logs: `context-core-stage-verification.json`,
`context-core-clippy.log` and `context-codegen-check.log` in the external artifact folder.

The renderer continuation now matches **5,832 C++ outcomes**, comparing exact output
or raw exceptions: 4,608 single-byte cases and 1,224 Unicode/layout cases across six
formats, three name/value fields and trees with children. **32 common release tests**
pass, including a 372-case frozen C++ golden subset; scoped all-target Clippy with
warnings denied passes. This verifies the native tree component, not complete public
EXPLAIN/expression/value integration or shell behavior.

RenderTree/RenderParams/output now own bytes and rendering returns Result. Text layout
uses 13,032 bytes of frozen utf8proc Unicode 15.1 properties and Rust grapheme rules;
no C++ is linked. The port matches C++'s invalid-value omission, JSON validation/NUL
handling and uppercase control escapes, empty-line splitting, ASCII-CI first-value map
semantics and target-C-char Mermaid trimming/splitting behavior. Reproducible offline
export/oracle sources are in scripts/; binaries, full outcomes and earlier mismatch
logs remain on the external SSD. Original raw-name databases remain unchanged.

A read-only C++ journey confirms raw-schema SELECT succeeds while SHOW TABLES FROM
that same schema fails with invalid unicode in value construction; the UTF-8 control
succeeds. SHOW now validates its generated VARCHAR filter literal at that actual value
boundary, while catalog/search-path identities retain bytes. Rust public SQL/shell
rechecks remain pending the full build. Renderer evidence: renderer-checkpoint.json under the same external artifact folder;
the following function stage advances the build further. All language clients and final performance acceptance remain
open; keep the uncached lower-memory controls unchanged.

The built-in-function/ENUM continuation passes **197 affected release tests**, with the
same one pre-existing exhaustive cast grid ignored. Types/function/built-ins and
storage/catalog/planner all-target Clippy pass with warnings denied; codegen checks pass.
ENUM dictionaries and codecs now retain bytes through casts, UNION tags and native SQL
rendering. **8,735 C++ ENUM comparisons** match exact wire data, rendered bytes or errors,
including 560 expected exception outcomes. The 50-case golden subset is checked in.

Two new C++ databases (raw ENUM label and UTF-8 control) load through the signed Rust
catalog/storage SDK. Rust appends the second ENUM code on copies, checkpoints them,
and both Rust and C++ reopen with the expected two rows and unchanged label bytes.
Original hashes stay unchanged; the eight older raw-name fixtures also pass read-only
checks. This verifies checkpoint interoperability, not Rust shell or WAL recovery.
Report pointer: enum-file-roundtrip-path.txt in the external artifact folder.

STRUCT/UNION lookups use native ASCII folding; regex group/result names, VARIANT keys,
summary/repeat names and log messages retain bytes. Scalar bind-expression callbacks
can propagate native errors: typeof constructs its VARCHAR while binding; alias keeps
its argument name as bytes and constructs VARCHAR at execution. C++ direct/pruned
journeys confirm that distinction; the new Rust component tests cover the callbacks
and kernel, while full Rust SQL pruning journeys remain pending.

That stage stopped at 25 optimizer diagnostics; the next continuation closes them.
Evidence/hashes: functions-enum-checkpoint.json. Remaining native expression/value,
optimizer/execution/result/logger/parser work, public shell/corpus/file/WAL gates,
selected language clients and final lower-memory performance acceptance remain open.

The optimizer/path continuation passes **187 affected release tests**, all-target
Clippy with warnings denied, and codegen checks. Join-statistics names, late row-id
rewrites, CTE result names, STRUCT/VARIANT paths and ENUM comparisons retain bytes;
STRUCT lookup follows ASCII folding. Rewrites that create VARCHAR constants validate
at C++'s actual Value-construction boundaries. The table-file known-value callback now
accepts byte column names; downstream CSV/JSON/Parquet/runtime adapters still need the
full-build integration.

ColumnIndex and StorageIndex now own raw VARIANT field-name bytes and have native
binary decoders. Generated property structs use ByteString string properties, not a
generic byte-list encoding. **10,482 C++ path comparisons** match exact metadata and
wire round trips across leaf, nested and pushed-down-extract paths; a 42-record golden
subset is checked in. The C++ oracle seeds field objects via its native deserializer
to initialize the otherwise unused/uninitialized index scalar in field constructors.
These are component/codec checks, not public query, storage-scan or WAL acceptance.

That checkpoint stopped at execution; the following stage closes those library errors. Evidence:
optimizer-path-checkpoint.json in the external artifact folder. The full workspace,
public shell/corpus/file/WAL gates, clients and final performance acceptance stay open.

The execution continuation passes **170 affected release tests** (the same pre-existing
exhaustive cast-grid test remains ignored), scoped all-target Clippy with warnings
denied and codegen checks. Execution and its targets type-check. Constraint/ALTER/DDL
names and errors, cast source-column context, CTE/window/pivot/copy metadata, scan
projections and physical plan labels retain bytes. Export SQL is written as bytes and
Value SQL-formatting errors propagate. EXPLAIN and profiling rendering return Result;
full public profiling/SQL timing and error journeys remain unverified.

The operator logger owns byte parameters and constructs checked VARCHAR Values at the
C++ boundary. Normal hash-join logging paths propagate Result; cleanup Drop explicitly
ignores its result. Parquet row-group logging helpers were adjusted to propagate
errors too; Parquet still awaits full-build integration. **3,494 C++ physical projection/
log comparisons** match exact parameter bytes, text or raw exceptions, including 140
expected invalid-Unicode errors; a 16-record independent golden subset is checked in.
These use real physical projection objects, not mock operators or a C++ engine fallback.

The serial release build took 10m52s and was resumed through the same live process,
without restart/overlap. That stage stopped at 25 CSV diagnostics; the following stage
closes those library errors. Evidence: execution-native-checkpoint.json in the external
artifact folder. No new public CLI/C API/client or full corpus/WAL/performance acceptance
is established.

The CSV continuation now compiles all targets and passes scoped Clippy with warnings
denied, codegen checks and **two release integration tests** (104 frozen C++ records).
Column names, supplied column/type maps, COPY names, multi-file union names and schema
errors retain bytes. Cast/sniffer errors and option/prompt buffers retain native names.
Option keys use ASCII matching; header collision folding now follows ASCII semantics.
`ToNamedParameters` propagates checked VARCHAR construction errors, while reject-table
append buffers retain bytes like C++'s direct string_t appends.

**12,228 exact C++ outcomes pass**: 10,482 metadata/error/VARCHAR-boundary comparisons
(six operations × 1,747 names), plus 1,746 real file scans through C++'s table-function
relation and Rust's direct read_csv binder/scanner. All scan values match; 140 expected
invalid-Unicode exceptions from ToNamedParameters match exactly. One empty-name public
C++ scan is preserved separately: its public binder generates C0, whereas the direct
Rust reader SDK exposes the original empty name. It is excluded from the SDK comparison;
public Rust SQL must verify that binder boundary. This is not complete public CSV/shell
acceptance. Native filesystem paths, raw parser ingress and full runtime/reject-table
journeys remain open.

The whole-workspace check at that checkpoint stopped at 213 autocomplete diagnostics.
Evidence: csv-native-checkpoint.json, csv-name-reference-sdk.tsv,
csv-workspace-check-1.jsonl in the external artifact folder. The following core stage
advances type/SHOW names; autocomplete remains unverified.

The unbound-type/SHOW continuation passes **138 scoped release tests**, none ignored,
across types/AST/parser/planner/CSV. All five packages pass all-target Clippy with warnings
denied, and codegen --check passes. UnboundTypeInfo, TypeParameter, TypeExpression and
ShowRef name fields retain bytes; borrowed aliases explicitly serialize as STRING bytes.
SHOW's generated VARCHAR literals validate at the actual C++ value boundary.

**34,940 C++ comparisons pass**: ten modes × 1,747 names × legacy/current serialization.
They compare exact wire bytes, native SQL text and raw binding errors, covering type
names/qualifiers/field aliases, ENUM parameters, direct default binding and SHOW fields.
A 300-record golden subset is checked in. A real legacy-default-binding difference was
fixed: an empty parameter alias uses ParsedExpression::GetName, which falls back to
native expression SQL. Direct C++ verification confirms mixed explicit/empty aliases
produce STRUCT("INTEGER" INTEGER, b VARCHAR); the old test expecting rejection now
asserts that exact structure. The initial failing test log is preserved.

This is metadata/component verification, not full type-rendering parity. The old
LogicalType::to_bytes unbound case and infallible rendering wrappers still need native,
fallible integration. Raw SQL parser ingress and public SHOW/SQL/shell journeys remain
open. Autocomplete candidate/result byte carriers and PEG name integration are WIP:
the fresh all-target check stops at **73 transformer diagnostics**; later workspace
packages remain unchecked by a successful full build. Do not decode native names to
satisfy old text signatures. Evidence: unbound-show-native-checkpoint.json and
autocomplete-native-check-7.jsonl in the external artifact folder.

The low-memory cache policy is unchanged. Python and all other selected clients, final
corpus/shell/file/WAL verification and documented final performance acceptance remain open.

Autocomplete now passes its all-target compile check, and selected autocomplete/types/
AST/parser/planner all-target Clippy with warnings denied passes. Its original 73
transformer errors are resolved through native identities and genuine PEG-token encoding,
without decoding catalog names to text. FunctionExpression/WindowExpression qualifiers
and names, CollateExpression/StringTypeInfo collation and LoadInfo metadata now retain
bytes. Function/window dispatch and collation resolution use ASCII byte semantics;
actual VARCHAR construction in USE/PRAGMA/ORDER BY propagates validation errors.

The autocomplete continuation now passes **154 signed release tests**, zero ignored:
152 affected types/AST/parser/planner/autocomplete tests plus two public completion
bind/scan journeys. PEG journeys cover Unicode INSTALL VERSION/repository, replacement
and table aliases, CTE aliases and named windows. Selected packages and optimizer pass
all-target Clippy with warnings denied; codegen --check passes.

**20,964 C++ outcomes match** exact function/window/postfix/collation/LOAD wire bytes and
native SQL. A 180-record golden subset is checked in. LOAD SQL formatting now matches
the native component, including its omission of version and single-quoted repository.
The initial window probe used constructor INVALID frame boundaries, which C++ refuses
to render. Its corrected name matrix uses the normal unbounded-preceding/current-row
frame in both implementations. Invalid-frame exception propagation remains an open
fallible-AST-rendering gate; the original failed reference/log is preserved.

The public sql_auto_complete bind/scan path with real catalogs matches direct C++
journeys: Unicode démo emits the quoted identifier plus dot, while raw byte FF fails
with the exact invalid-Unicode exception at VARCHAR construction. This verifies that
specific public function path, not the final Rust SQL/shell or every completion ranking.
The earlier **34,940 unbound/SHOW outcomes** still pass after the shared AST changes;
parser regression is **8,076 matched outcomes**, zero mismatches, with **700 invalid-
UTF8 inputs still pending** raw ingress and not counted as passed.

The fresh whole-workspace/all-target check passes autocomplete and optimizer, then
stops at **53 Parquet diagnostics**. Runtime, clients and final public binaries remain
unchecked by a successful full build. Evidence: autocomplete-native-checkpoint.json,
autocomplete-stage-workspace-final.jsonl and their hashed logs/source snapshot on the
external SSD. Final SQL/shell/corpus/C API/storage/WAL, raw parser/filesystem and old
fallible-rendering boundaries, Python and every selected client, and documented final
speed-memory acceptance remain open. The uncached memory baseline is unchanged.

The Parquet continuation now compiles all targets and passes scoped Parquet/function/
execution all-target Clippy with warnings denied and codegen --check. **31 release tests
pass**; two pre-existing Snappy comparison/benchmark tools remain ignored. Schema/path
Thrift strings, reader/writer/COPY/scan identities, FIELD_IDS/shredding keys, VARIANT
dictionaries/objects, enum labels and written-statistics names preserve bytes. Actual
VARCHAR and GeoParquet JSON boundaries retain C++ validation behavior; plan KV metadata
uses STRING bytes rather than lossy UTF-8 conversion.

**6,988 new C++ comparisons pass**: 5,241 exact Thrift schema/path/encryption-path wire
round trips and 1,747 actual GeoParquet JSON/error outcomes, including 140 expected invalid-
UTF8 serialization errors. Geo JSON comparisons are semantic values, not object-key text
ordering. Sixty golden records and a separate all-256-byte VARIANT key test pass. Geo JSON
keys use the C++ c_str NUL view; primary_column retains its explicit length.

Two real C++ files (raw caf-E9 and UTF-8 café top-level/nested names) load through the
signed Rust Parquet SDK with exact names and values. Rust rewrites copies and reopens
them; C++ sees identical original/rewrite outcomes and the original hashes are unchanged.
Both files count two rows. UTF-8 scalar/nested data match exactly. Raw scalar/full nested
materialization throws the same invalid-Unicode value error on both original and Rust-
written files; this is preserved as expected C++ behavior, not counted as successful raw
public SQL materialization. Final Rust public SQL/shell parity at this boundary remains
open. Exact journey evidence: parquet-file-roundtrip-path.txt and its verification-final.json.

COPY RETURN_STATS retains native column identities until checked VARCHAR key construction.
ICU static output labels and a genuine-text mutable reborrow now compile; no new ICU
runtime acceptance is claimed. The fresh workspace check passes these packages and stops
at **13 TPC-H diagnostics**. Evidence: parquet-native-checkpoint.json and
parquet-stage-workspace-check-4.jsonl on the SSD. No final full-engine/corpus/shell/C API/
storage/WAL, selected-language-client or performance acceptance is established by these
component tests. Remaining raw parser/filesystem/fallible-rendering and broader Parquet
features stay open; the uncached memory baseline remains unchanged.

The TPC generator continuation compiles TPC-H and TPC-DS libraries and all selected
targets. Catalog/schema/suffix identifiers remain bytes through generator binding,
schema creation and append lookups. FunctionData::modified_database and planner
statement modification lists now retain database-name bytes; known runtime sequence/
table-function callback signatures were updated, but the full runtime still awaits the
complete workspace build. Static query/result labels encode genuine text at AST edges.

Two signed release integration tests pass (one per generator), each exercising actual
scale-zero bind/scan and table creation with raw catalog/schema/suffix names and UTF-8
controls, including exact modified-database identity. Four direct native C++ schema
creation journeys confirm raw/control behavior for both generators and empty tables.
The C++ probe must register the target in MetaTransaction before invoking the native
wrapper; initial missing-registration failures are preserved as harness evidence.
These checks establish native schema/name handling, not generated row/data parity or
fresh benchmark acceptance. Scoped TPC-H/TPC-DS/function/planner all-target Clippy with
warnings denied and codegen --check pass.

The latest whole-workspace check now stops at **35 JSON diagnostics**, covering JSON
creation/transformation, scan names and native VARIANT integration. Evidence:
generator-native-checkpoint.json and tpch-native-workspace-check-5.jsonl on the SSD.
Selected client bindings, full public SQL/shell/corpus/C API/storage/WAL, raw parser/
filesystem and fallible rendering, and final signed-binary speed-memory acceptance
remain open. No memory-increasing table cache was restored.

The JSON continuation now compiles all targets and passes scoped JSON/function/planner
all-target Clippy with warnings denied and codegen --check. **68 signed release tests
pass**; four pre-existing timing/exhaustive-oracle tests stay ignored. Native scan names,
STRUCT/UNION transform lookups, VARIANT dictionary keys and error metadata retain bytes.
JSON create functions and casts validate STRUCT/UNION name constants at the actual C++
bind-time VARCHAR construction boundary, propagating errors before kernel execution.

**10,482 new C++ comparisons pass**: 5,241 actual relation-query outcomes for STRUCT/
LIST<STRUCT>/cast-to-JSON (including 420 matched invalid-Unicode rejections), and 5,241
native COPY projection renderings for integer/date/timestamp inputs. Ninety golden
records pass. A separate test checks 128 escaped-control/ASCII keys plus two Unicode
keys through JSON-to-VARIANT dictionary conversion; this test is not a cross-engine
file or full VARIANT acceptance claim.

COPY plan rewrites now carry native ParsedExpression trees (function crate depends on
AST), matching the actual C++ copy_json.cpp implementation. Positional references,
strftime format constants, aliases, struct_pack and to_json are constructed directly.
An auxiliary experiment parsing generated text exposed empty/NUL syntax errors in the
old Rust adapter; those are not C++ COPY requirements and are not counted as parity
failures. The final native C++ matrix includes those names and matches. Raw SQL parser
ingress remains separately open.

The fresh full-workspace/all-target check now stops at **14 Arrow diagnostics**. Evidence:
json-native-checkpoint.json, json-name-compare-1.log, json-copy-native-compare-1.log and
json-native-workspace-check-1.jsonl on the SSD. This verifies native JSON component paths,
not complete public JSON file/SQL/shell/corpus behavior. Arrow/client APIs, the remaining
runtime/C API build, full correctness/storage/WAL and documented final performance
acceptance remain open. The uncached lower-memory baseline is unchanged.

The Arrow native-name continuation compiles all targets and passes all-target Clippy
with warnings denied and codegen --check. **11 signed release tests pass**, none ignored.
Imported schema names use the actual Arrow C-string byte view, preserving invalid UTF-8
and truncating at NUL; exported names, nested STRUCT/UNION fields, projected columns and
DuckDB table-schema names retain native bytes. ENUM dictionary export uses full label
byte lengths rather than text carriers. Alias lookup compares raw type-name bytes.

**6,988 exact C++ comparisons pass**: 5,241 schema export/root-import/nested-import
outcomes plus 1,747 ENUM array dictionary outcomes. These compare exported C names/
formats/flags, imported duplicate/empty names, exact nested DuckDB type serialization,
dictionary offsets/data and array indices. C-string NUL truncation differs deliberately
from the dictionary's length-preserving byte data, matching C++. Sixty golden records
are checked in. This is component C ABI schema/data evidence, not complete Arrow/client
acceptance: format and extension-metadata text codecs still need their own boundary audit.

The fresh full-workspace/all-target check now reaches the main rustdb crate and stops
at **205 runtime diagnostics** across result/logging/database/catalog/system-function
consumers. Evidence: arrow-native-checkpoint.json, arrow-native-workspace-check-1.jsonl
and runtime-native-diagnostic-index.json on the SSD. Completing these runtime names is
required before final public binaries, shell/corpus/C API/storage/WAL and selected-client
verification can run. Python and all other clients, raw parser/filesystem/fallible
rendering, and documented final speed-memory acceptance remain open. The lower-memory
uncached baseline is preserved.

Detailed stage logs, exact limitations, source hashes and resume pointers live in
`.context/identifier-bytes-progress.md` and the external artifact folder
`.context/artifacts/identifier-bytes-20261023/`. Do not launch agents or overlap heavy
workloads to finish this integration.

## Where things stand

- **Corpus**: the latest complete serial sweep has **4781 of 4870 pass**, 15 fail, 74 skip;
  C++ is 4795 pass, 1 fail, 74 skip. All remaining Rust failures are the 15 accepted
  divergences in `conformance/divergences.md` (11 C++ extension binaries, 4 expected
  memory errors Rust avoids). No unexplained regressions, crashes or timeouts.
  - The fresh callback/error-byte sweep plus the uncommitted cache flag uses normal thread
    defaults and passes the 4855-file ratchet without rechecks, shrink or verdict changes.
    Every skip reason matches the prior baseline; the 15 failure messages match too,
    apart from a process ID in a temporary filename. `target/slt/rust.jsonl` is refreshed
    from this full run, with its previous contents backed up beside the raw results.
  - An earlier 4870-file sweep ran with one worker and `SLT_THREADS=1`: 4779 pass,
    17 fail, 74 skip. Two tests require normal thread defaults (`test_batch_limit_filters`
    expects a parallel LIMIT, `reset_threads` compares startup threads with RESET); both
    pass on serial rechecks without the override. C++ reproduces the same one-thread
    behavior. That older baseline incorporated two rechecks; its raw results remain in
    the ICU artifact folder. The current canonical results need no reconciliation.
  - The ICU gap is closed (`2fa715c`): all 130 collations match C++ sort-key digests
    across 484,110 strings; all 24 collation files pass. The second gain over the
    previous 4779 baseline is a formerly skipped `require disk_space 40gb` TPC-H
    grouping test, freshly passed by both Rust and C++ as well.
  - `json_serialize_plan` is ported (1486 of 1524 plans byte-identical, work-queue 8).
    Database encryption is ported (`crates/crypto`); Parquet encryption remains open.
  The ratchet `conformance/passing.txt` lists **4855** files (4781 passes + 74 shared
  skips), with zero regressions and no shrink. The C++ reference's previously skipped
  grouping result and `conformance/dashboard.md` are refreshed.
- **Concurrency:** snapshot filtering and lost partial-write undo records are fixed
  (`506354f`). All 100 original stress runs, 20 diagnostic runs and the fresh full sweep
  pass. The earlier `max_swap_space_error.test` verification passed 20 of 20. Rerun a
  file alone and inspect its real failure before attributing it to a change.
- **C API:** 546 of 547 functions; all 30 `test/api/capi` files pass
  (`scripts/capi-tests/run.sh`, `--lib cpp` runs them against the C++ library). ADBC is
  missing. Fresh public binaries on `506354f` also pass 40 C API concurrent-write
  stress runs with the runner's zero-byte checkpoint threshold; 40 preserved-binary
  and ten C++ controls pass. The explicit-transaction lock sequence matches C++.
- **CLI shell (the owner's top priority; engine gaps remain):** `crates/shell` (`rustdb`) is DuckDB's
  shell one to one, linenoise included: all 392 `tools/shell/tests` that pass on the C++
  CLI pass (`scripts/shell-tests.sh --binary rust|cpp`), and 764 of 768 cases of
  `scripts/shell-diff.py` (modes, types, dot commands, options, errors, pty sessions)
  match byte for byte; the four others are known engine differences (work-queue item 3).
  Both suites were reverified after the ICU merge with one worker. Upstream shell tests
  leave temporary paths unquoted; `pytest-ssd` in the ICU artifact folder exposes
  `tmp_path` through the workspace symlink so all test files remain on the SSD.
  `duckdb_functions()` matches DuckDB's exactly.
- **Compression:** `crates/zstd` ports zstd 1.5.6's encoder and decoder (zstd's frames at
  levels -5..3; ruzstd is gone); roaring, hugeint bitpacking and partial blocks match
  DuckDB's layouts (work-queue item 5 lists what is left).
- **Performance** (`docs/perf-backlog.md`, fresh round 6 baseline; speed = C++ time ÷
  Rust time, memory = Rust ÷ C++; 160 result checks pass):

  | suite | speed 14 / 1 threads | memory 14 / 1 threads |
  |---|---|---|
  | TPC-H SF10 | 0.88× / 0.94× | 0.29× / 0.21× |
  | ClickBench | 0.87× / 0.91× | 0.36× / 0.29× |
  | h2o group-by | 1.03× / 0.99× | 0.72× / 0.40× |
  | h2o joins | 1.16× / 1.26× | 0.79× / 0.50× |

  H2O joins meet the revised gates on this run; the other suites have individual slow
  queries. ClickBench database-open q00 uses about 3.2 MB extra peak RSS (1.11× above
  the interpreter). The owner accepts this lower-memory tradeoff as the target baseline; final-binary
  performance acceptance remains open. Persistent table caching is rejected under the
  2026-10-23 policy, and the uncached source behavior is restored.

## Saved work (2026-10-22)

No previous Rust build, corpus run or agent process was active when this session resumed.
The ICU worktree (`worktree-agent-a57faf12816ee7338`) has been merged; keep it and its
artifacts intact. Performance round 6's five saved commits (`1814548` through `155fe70`) are
integrated as `2a4541a` through `1e53786`. They pool vector buffers by size, speed
RLE skips, keep all-row selections implicit, reuse compressed string heaps, and reuse
expression scratch lists and constants. A retained-result lifetime regression test was
added (`547a13b`). All 202 focused release unit tests pass; workspace/all-targets check
and Clippy with warnings denied pass. The focused SQL run has 200 passes and three
shared skips (203 files), with no failures, crashes or timeouts and zero regressions
against the 4855-file ratchet. These focused rechecks retain the prior overall
4781/15/74 results; the subsequent full sweep on `506354f` plus the cache flag confirms
them without rechecks. Shell verification passes with the same five known differences; all eight fresh
performance suites are complete with 160 passing result checks. Final-binary performance acceptance
remains open under the lower-memory policy in `docs/performance-acceptance.md`; that
policy supersedes the original independent speed and memory targets.

The benchmark harness now fails on mismatches or engine errors and retains its
check verdicts in JSON (`65042a1`); six regression tests pass. Older JSON lacks those
verdicts and can conceal failures: saved `r6base-hits-t1.txt` includes a C++ q10 open
failure, so that aggregate is incomplete. New evidence lives under
`.context/artifacts/parity-20261022/`, including the pre-change binaries, unit logs,
focused SQL results and ratchet check. Global rustfmt also finds pre-existing formatting
in untouched files; the integrated code was formatted without unrelated churn. Its brief is `.context/briefs/perf-round-6.md` (base `4dca105`).
Do not launch the historical briefs in parallel: the owner's latest limit is one task.

ICU verification artifacts are under
`.context/artifacts/icu66-resume-20261022/` (the workspace's dedicated external SSD
folder). `differential.py` reruns both engines serially against the saved Unicode inputs.
The six ICU unit tests, workspace/all-targets check and Clippy pass without warnings.
Re-exporting with the saved offline tool reproduces all 2,680,616 data bytes (SHA-256
`9b5516e7609c98b7e25b6ed27ae60ae2ae49ebe1386fa8ef3bccb37b9df3d5ac`).
`scripts/icu-collation-data.sh` and `scripts/conformance.sh` default to `JOBS=1`
for subsequent exporter builds and corpus runs.
The port includes a deliberate termination guard: on rare inputs where ICU's FCD iterator
loops forever, Rust collates the NFD form instead. Such reference hangs are excluded from
byte comparisons; a Rust unit test covers the fallback.

## Historical cache experiment (2026-10-22; rejected 2026-10-23)

Current policy: retain the uncached lower-memory baseline. The uncommitted cache switch
has been removed; its patch/source and uncached binary identities are preserved under
`.context/artifacts/performance-policy-20261023/`. The following investigation is a
historical log. Statements below about an active/uncommitted cache candidate describe
that earlier experiment, not the current source policy.

The working tree retains the uncommitted persistent-table cache experiment in
`crates/storage/src/table/column_segment.rs`. Modern plain/constraint ART disk restoration
and its publication fix are committed and pass the fresh full sweep; the remaining
paths stay open under work-queue 16. Candidate checks and both 160-query benchmark comparisons are saved under
`.context/artifacts/parity-20261022/cache-policy/`. All candidate shell/C API checks
and result comparisons pass, but the cache policy is **not accepted**: q21, q22 and
q28 have memory above C++, despite improved average speed. See the experimental table
in `docs/perf-backlog.md`. Preserve the uncached binaries in `uncached-merged/`.

The initial cache-only full sweep completed at **4780 pass, 16 fail, 74 skip**, zero crashes or
timeouts, with exactly one changed verdict against the prior baseline. The ratchet
correctly fails with one regression, `test/sql/pragma/profiling/test_profiling_fs.test`
(zero `TOTAL_BYTES_READ` on SELECT following reattach). A fresh C++ native-unittest
control passes, the preserved uncached Rust control passes, and the cache candidate
fails again in isolation. Source rebuilds plain indexes while loading the catalog,
warming table blocks at ATTACH. Native persisted-index restoration is work-queue item
16; `.context/index-loading-regression.md` records the investigation. Do not change
profiling counters or tests to hide this gap. The canonical ratchet/results were not
replaced by the failed candidate sweep.

The full-run path is recorded in `cache-policy/full-run-path.txt`; all worker logs and
`comparison.json` are retained. No workload is active after the serial allocation
diagnostic completed. Its report is `cache-policy/alloc-diagnosis/report.json`: cold
single-thread q21 and q28 retain similar table bytes to C++, with approximately 99 MB
and 350 MB extra peak-minus-retained-table memory respectively. This is an allocation
estimate, not a precise heap attribution. q22 used less memory than C++ in this single
CLI run despite its repeated C API benchmark outlier, so cache/filter-history effects
need controlled investigation. Fix the persisted-index regression first, then diagnose
or reject/rework caching's memory outliers before accepting the policy or claiming parity.
The previous signed binaries are the cache candidate; `uncached-merged/` preserves the
other verified variant. The native ART restoration build is complete and signed
(`.context/artifacts/index-reload-20261022/`). All 53 storage unit tests pass, including
modern/deprecated-leaf lazy reload and post-reload mutation. The profiling regression
passes again; 384 affected SQL files pass with nine shared skips (393 total). Ten
cross-engine file cases pass across storage 1.0/1.2/1.5 and GCM/CTR, including mutation,
reopen, cold ART residency and constraints. C++ encrypted fixture writes use its test
switch `force_mbedtls_unsafe`; normal C++ readers work without it. Workspace Clippy
with warnings denied, 30 C API files and 392 shell tests pass. The new full sweep
finished at 4779 pass, 17 fail and 74 skip, zero crashes/timeouts. Exactly two concurrent
checkpoint files regressed; all other verdicts match the canonical baseline. Its unique
path is in `index-reload-20261022/full-run-path.txt`, with `comparison.json` and worker logs.
The first full run of native reload exposed two concurrent checkpoint regressions:
`concurrent_checkpoint_insert.test` and `concurrent_checkpoint_wal_index.test_slow`
read newly serialized index locations before partial blocks were flushed. The
publication fix now requires an explicit writer flush before returning while the
bound-index guard or unbound-storage mutex is held, matching C++ lock/flush ordering.
All 55 storage unit tests pass, including deferred-write and failed-flush retry checks.
Both original concurrency files pass three targeted trials and again in the expanded
451-file cohort (441 passes, ten shared skips, no failures/crashes/timeouts). All ten
cross-engine file cases, workspace Clippy with warnings denied, 30 C API files and
392 shell tests pass on the fix. The old failing full results remain preserved.
The fresh full sweep on `cc5d2c2` plus the uncommitted cache flag finished at **4780
pass, 16 fail, 74 skip**, with zero crashes/timeouts. Both checkpoint regressions pass
in this sweep. The sole unexpected failure is now
`test/sql/update/test_stress_update_issue_19688.test`: the final concurrent
UPDATE/DELETE/INSERT check finds 999 distinct IDs rather than 1000. The ratchet fails
with one regression; canonical results and the 4855-file passing list remain unchanged.
The unique run path is in `index-reload-20261022/full-run-path.txt`; its `comparison.json`
and worker logs preserve the failure. Do not promote this failed run.

Serial repeated checks reproduce the candidate failure twice in 70 runs (68 passes).
The preserved uncached binary passes 69 of 70, with one earlier concurrent-iteration
failure whose abbreviated worker result does not establish the same cause. C++'s native
unittest passes all 20 controls. This is an unresolved concurrency issue, not a proven
cache or index-reload regression. Full logs for the additional 50 checks of each Rust
binary and JSON summaries are in `index-reload-20261022/stress-update/`. No workload is
active after those repeats. Diagnose the missing row before further parity acceptance.

The subsequent investigation reproduced two concurrency defects deterministically.
Column filtering re-read a growing vector's row count after the scan fixed its snapshot
limit; a new match in the final partial vector could be emitted with an old row ID.
Filtering now receives and respects the scan's maximum count. Separately, a conflict in
a later vector, column, validity mask or struct field discarded undo records for earlier
writes. Mutation records now propagate through errors and enter the transaction's undo
log before the error returns. This preserves C++'s behavior: an aborted explicit
transaction keeps earlier writes locked until `ROLLBACK`, then releases them. A live
C++ C API control confirms that lock sequence (`aborted-locks-cpp.log`).

All **80 storage tests pass** (55 unit, 14 MVCC, nine constraints, two type tests), and
workspace/all-targets Clippy with warnings denied passes. The six new regression tests
cover matching/nonmatching snapshot tails, conflicts across vectors and row groups,
multiple columns, NULL validity and struct fields. Each newly exposed defect failed
before its fix; the mutation tests also verify locks remain held before rollback.
The final MVCC rerun after the test-helper lint fix passes all 14 tests.

Release stress checks pass: **100 of 100 original runs and 20 of 20 diagnostic
runs**. The affected SQL cohort has **374 passes and one shared skip** (375 files),
zero failures/crashes/timeouts, and zero regressions against its scoped ratchet. The
first driver invocation incorrectly checked the subset against the entire ratchet and
stopped on missing unrelated files; the corrected scoped check passes without changing
the canonical list or rerunning the already completed stages.

The fresh full sweep is complete: **4781 pass, 15 accepted failures, 74 shared skips**,
zero crashes/timeouts, and **4855 listed, zero ratchet regressions**. The profiling,
both checkpoint and missing-ID stress regressions all pass. Every file's status and
skip reason matches the baseline; accepted failures match after normalizing one
temporary process ID. Raw results, worker logs, `comparison.json` and the prior canonical
result backup are in `full-20261022T211412Z-4173` under the index-reload artifact folder.
The canonical results are refreshed; dashboard counts and the passing list are unchanged.

The serial driver
`index-reload-20261022/stress-update/verify-journal.py` builds and signs the runner,
checks 100 original stress trials plus 20 diagnostic trials, checks the affected SQL
cohort and ratchet, then starts a fresh full sweep only if those gates pass. It stores
the source/binary manifest and all logs under `stress-update/verification/`. Poll the
live handle recorded in `.context/stress-update-investigation.md`; never launch a
duplicate workload. The previous failed full results and canonical ratchet remain
preserved. Shell/C API binaries still require rebuilding and
verification on the final source; an earlier partial release build was deliberately
stopped after the runner linked, before finishing the remaining packages.
The completed sweep ran through `resume-after-cohort.py`, which verified the
unchanged runner hash and source, checks the scoped ratchet, then resumes at the full
stage. Its unique directory is recorded in `index-reload-20261022/full-run-path.txt`.
`verify-post-full.py` completed successfully after that review. Fresh signed public
binaries pass all 30 C API files, 392 shell tests/three shared skips and the 727-case
shell comparison (zero unexpected differences, the same five known gaps). Forty C API
stress runs with the SLT checkpoint settings pass, as do 40 preserved-binary and ten
C++ controls. The live three-connection explicit-transaction lock sequence matches
C++ exactly. Its unique output directory is in `stress-update/post-full-path.txt`,
with `verdict.json` and signed identities. Both harnesses re-sign their public binary
before testing, so the manifest retains initial and final signed-file hashes. The SLT
hash remains the hash used for the full sweep. Both verification drivers are terminal;
no build or test workload remains active at this checkpoint.

The lazy type-catalog gap is now ported and verified. Builtin entries are real internal,
temporary, committed catalog objects: first lookup preserves spelling, scans create
the remaining lowercase defaults, and failed binds/rollbacks retain the same state as
C++. Suggestions compare the catalog map's original keys, including invisible versions.
Type binding follows C++'s fallback and qualification rules, keeps builtin modifiers,
and distinguishes type scopes from view scopes by default-schema precedence. This
fixes builtin shadowing and the cross-catalog ALTER enum fallback without weakening
the test. `duckdb_types()` reads actual entries and uses logical type IDs for internals.

Validation: six catalog unit tests, workspace/all-targets Clippy with warnings denied,
36 new unmarked byte-for-byte shell cases, the full 763-case shell comparison (zero
unexpected differences, five existing known gaps), and 882 affected SQL files (875
passes/seven shared skips, zero regressions). Two enum/schema/index files created by
opposite engines pass mutation/checkpoint/read-only reopen in both directions, with
unchanged hashes for read-only checks. The fresh full sweep has 4781 passes, 15 accepted
failures and 74 shared skips: every status, skip reason and normalized failure message
matches the baseline; the 4855-file ratchet is unchanged and passes.

Fresh public binaries pass all 30 C API files and 392 shell tests/three shared skips.
An 18-query, three-connection C API sequence matches C++ exactly, including enum vector
retrieval, connection-local defaults, rollback, shared types and schema precedence.
The comparison helper initially dereferenced a null legacy enum fetch; it now uses the
chunk/vector API on both engines. That helper fault did not require engine changes.
Artifacts: `.context/artifacts/shell-type-catalog-20261023/` (the final full,
verification and public directories have pointer files and manifests). The canonical
results are refreshed with a preserved prior copy; all drivers are terminal and no
build/test workload remains active. Remaining shell gaps and the cache performance
experiment stay open. `.context/shell-type-catalog-investigation.md` has the detailed
source findings and verification history.

Native disk restore covers plain and stored constraint indexes so far; expression/WAL,
pre-v0.10 and fallback paths remain open under work-queue 16. The cache policy is still
uncommitted and unaccepted because its memory/performance gates remain unresolved.
Details: `.context/index-reload-progress.md`.

The raw-byte shell work remains open. Its first shared boundary is implemented:
`Value::try_varchar(&[u8])` validates bytes with DuckDB's value-construction error
classification; the C API constructor and appender's fallback use it. Unchecked string
storage and the appender's direct paths retain their existing validation timing. All
102 type/vector unit tests and workspace/all-targets Clippy pass. Fresh signed public
binaries agree with C++ on 65,823 byte sequences (18,445 accepted, 47,378 rejected),
including exact accepted-byte round trips and embedded NULs; all 30 C API files and
392 shell tests/three shared skips pass. This behavior-preserving constructor relocation
does not close the raw input, parser, identifier or error-byte gaps. Artifacts and
manifests: `.context/artifacts/shell-bytes-20261023/`; source/probe notes:
`.context/shell-bytes-investigation.md`. No job is active after these checks.

The initial scalar error-byte boundary (`983e3eb`) preserves error-message bytes: `Error` owns bytes and
provides native byte formatters; scalar C API bind/init/execute callbacks and SQL
`error()` keep invalid UTF-8 intact. Native C API error accessors use those bytes.
NUL sanitization runs before JSON conversion, and JSON control escapes use DuckDB's
uppercase hex digits. `scripts/capi-error-bytes.py` compares 2,128 cases with C++:
all byte values and selected sequences, callback phases and raw-vector SQL errors,
direct/prepared statements, plain/JSON errors and connection recovery. All match.
The 23 common unit tests, workspace/all-targets Clippy, 30 C API files, all 765 shell
differential cases (zero unexpected differences, five known) and 392 upstream shell
tests/three shared skips pass on fresh signed binaries. Two new unmarked shell cases
cover NUL/control/Unicode messages and recovery. The full 4,870-file sweep has 4,781
passes, 15 accepted divergences, 74 shared skips and no crashes/timeouts; every status,
skip reason and normalized failure message matches the prior baseline. The 4,855-file
ratchet has zero regressions or new entries. Canonical results are refreshed with the
prior copy preserved. Public/full manifests and pointer files are under
`.context/artifacts/shell-bytes-20261023/` (`raw-errors-path.txt` and
`raw-errors-full-path.txt`). Both validation drivers are terminal. This does not close
raw shell input, parser/name bytes or legacy text error wrappers. The cache experiment
remains uncommitted and unaccepted.

The remaining native callback error carriers are now byte-based too: table,
aggregate (including deferred errors), COPY, replacement scans and casts. The shared
cast/JSON transform/scanner path preserves byte messages, source-column context and
DuckDB's first-nonempty TRY_CAST error. Aggregate state-size errors now occur during
physical preparation after optimization; SQL PREPARE handles its contained plan too.
Pruned aggregates do not invoke the callback. Window aggregate state-size errors stay
at execution, matching C++. JSON-mode prepare no longer appends a caret to raw messages
beginning with `{`.

The expanded `scripts/capi-error-bytes.py --family all` oracle has 13,832 unique cases:
2,128 scalar, 2,128 table, 5,320 aggregate, 2,128 COPY, 532 replacement and 1,596 cast.
It covers all byte values/selected sequences, plain/JSON, direct/prepared statements,
empty-input/deferred errors, combine, pruning, SQL PREPARE, window timing, CAST source
context, TRY_CAST NULL results and recovery. All match C++ byte for byte. All 35 signed
function-library unit tests and workspace/all-targets Clippy pass, as do all 30 C API
files, 765 shell cases (zero unexpected differences/five known), 392 upstream shell
tests/three skips and the full 4,870-file corpus (4,781 pass, 15 accepted divergences,
74 shared skips, zero crashes/timeouts). Every status, skip reason and normalized
failure message matches the previous baseline; the 4,855-file ratchet is unchanged.
Canonical results are refreshed with the prior copy preserved. Both drivers are
terminal; manifests/pointers are in `.context/artifacts/callback-bytes-20261023/`
(`first-path.txt`, `final-path.txt`). Raw shell/parser/name bytes and legacy text
wrappers remain open, as do the other work-queue and performance gates. The original
cache experiment is excluded from this callback change and remains unaccepted.

Shell ingress now keeps byte-owned lines, SQL accumulation, echo and linenoise
history/completeness checks. Dot-command tokenization decodes escapes as bytes and
terminates each decoded argument at NUL, like C++ C strings. A byte callback interface
carries literal `.print` output without decoding; raw command-name lookup/errors also
retain bytes. Engine errors use the native byte formatter. Valid UTF-8 still uses the
existing highlighter; invalid-byte highlighting remains plain raw output until that
text-only boundary is ported. Other dot callbacks explicitly adapt to their existing
text state; SQL execution and argv still have legacy UTF-8 handoffs. This does not
establish raw SQL/parser/name/argv parity and adds no blanket UTF-8 SQL rejection.

The ingress step passes 14 shell and 23 common signed release unit tests plus
workspace/all-targets Clippy. All 778 cases in `scripts/shell-input-bytes.py` match C++
(all single-byte raw/escaped arguments and command names plus selected sequences).
All 768 shell differential cases have zero unexpected differences and four known;
392 upstream shell tests pass with three shared skips. `input-latin1-dot-print` is
now unmarked and passes; three additional unmarked cases cover octal/NUL handling,
quoted raw input and raw command-name errors. The full SQL corpus was not repeated
for this frontend step; the last accepted full sweep remains the callback-byte run.
Artifacts: `.context/artifacts/shell-input-bytes-20261023/verify-path.txt`.

Fresh C++ file probes expose the persisted-name boundary: raw table, column, schema
and enum-type names can be written/checkpointed/reopened by C++, while Rust rejects
all four with `Serialization Error: Failed to deserialize: string is not valid UTF-8`.
The corresponding valid-UTF-8 name controls pass on both engines, and every read-only
file hash stays unchanged. Fixtures/report: `shell-input-bytes-20261023/raw-name-files/`
under the external artifact root. The binary deserializer already has raw string-byte
access; typed name storage and its UTF-8 adapter need the remaining port. These
fixtures must be handled through actual byte-owning identifiers, without encoded
placeholder names or globally rejecting invalid SQL.

The typed byte-string codec foundation is verified separately. `ByteString` owns raw
bytes with checked UTF-8 access and no implicit text display. Binary properties and
in-memory JSON documents preserve those bytes; normal JSON text output retains C++'s
UTF-8 rejection. All 271 C++ property/default-property/JSON comparisons match, including
every single byte, embedded NULs and long strings. All 25 common and six serde unit
tests pass, as do workspace/all-targets check and Clippy with warnings denied. Evidence
and source hashes: `.context/artifacts/identifier-bytes-20261023/foundation-status.json`.
Actual parser/AST/catalog names are not migrated by this foundation; the four raw-name
file gaps remain open. Continue with persisted column names and their query-result path.

## How to work

- Build and sign: `cargo build --release -p slt -j 1 && scripts/sign.sh target/release/slt`.
- One file: `target/release/slt exec --engine rust --root .context/duckdb-download/duckdb-1.5.6 test/...`.
- Subset: `JOBS=1 scripts/conformance.sh test/sql/<dir> ...`; full:
  `JOBS=1 scripts/conformance.sh > "/Volumes/amirs ssd/ai context/conductor/duckdb-rust/logs/main/<name>.log" 2>&1`.
- The C++ reference CLI is `.context/duckdb-download/duckdb-1.5.6/build/release/duckdb`
  (run it from /tmp); its library is `.../build/release/src/libduckdb.dylib`. Use it to
  settle exact behavior and error texts and for cross-engine file round trips.
- Merging an agent branch: conflicts in `conformance/dashboard.md` take main's side (it is
  regenerated); check `conformance/passing.txt` stays sorted without duplicates; run
  `cargo check --workspace --all-targets -j 1`, the affected corpus directories, then a full run.
- Unit tests: build with `cargo test --workspace --release -j 1 --no-run`, sign the
  generated test executables, then run them serially with `--test-threads=1`. The older
  workspace baseline was 668+ passes; this continuation executed 80 storage tests and
  the changed ICU crate (6/6), not the entire workspace unit suite.

## Next steps, in order

1. Continue the remaining shell engine differences, the owner's top priority
   (work-queue 3). Lazy type catalog/suggestion parity, its full corpus and fresh public
   checks are complete. Finish the other native
   persisted-index paths (16); the profiling and concurrency regressions are fixed.
   Keep the rejected caching policy removed and preserve the lower-memory baseline;
   evaluate speed and memory together and document per-query outliers. Complete the
   Python/other client gates in `docs/language-bindings.md`. Work
   down the rest of `docs/work-queue.md` sequentially. ICU verification is complete; Parquet encryption (15) remains.
2. Keep one workload at a time. Use `JOBS=1` for canonical conformance runs; do not
   force `SLT_THREADS=1` globally because tests of thread defaults and parallel plans
   intentionally depend on the normal setting. It remains useful for focused diagnostics.
3. TPC-DS is ported (`crates/tpcds`, all 25 files pass). Its `dsdgen`, like TPC-H's
   `dbgen` in `crates/tpch`, carries TPC's license, not MIT — the owner should review the
   TPC terms before rustdb is published; both stay optional extension crates so the core
   is MIT.
4. Complete final performance acceptance against the approved lower-memory baseline,
   with result checks and per-query outlier reporting; exact C++ speed/memory parity is
   not required. Complete differential fuzzing and all selected language-client gates
   before M9 sign-off in `docs/porting-plan.md`.


The runtime continuation (2026-10-24) has reduced the scoped `rustdb --lib` check from
181 to **15 diagnostics**, in appender SQL construction and COPY DATABASE. This is a
compilation checkpoint, not a successful runtime/workspace build. Runtime result names,
search-path/default-database state, database modification lists, attached lookup/rename,
file-reader metadata and sequence names now retain bytes. Invalidation/checkpoint/WAL
failure messages and filesystem logging also retain bytes. Secret names and persistent
secret filenames retain bytes; the existing nontransactional/custom secret-file-format
gaps remain open. LOAD/INSTALL uses byte names, native Unix paths, binary STRING install
metadata and byte HTTP request paths; complete filesystem/configuration parity is not
established.

System-table VARCHAR construction is now fallible instead of decoding names for old
text interfaces. Its **bind-versus-scan error timing remains open**: the current runtime
collects rows eagerly during binding, whereas the C++ system scans construct their
Values during scanning. Profiling now constructs checked VARCHAR Values for extra-info
MAP keys/values at the C++ `CreateTree`/`MergeOperatorExtraInfo` boundary, and propagates
native renderer failures and byte output. These runtime paths still need public-journey
and oracle verification; compilation does not establish their behavior.

Scoped foundation verification passes **138 signed release library tests** (catalog,
function SDK, built-ins and storage), none ignored. All-target Clippy with warnings
denied passes those four packages plus planner; codegen --check passes. A further
signed file-logging test matches **8,735 frozen C++ cases**: four file-handle operations
for each of 1,747 byte paths, plus 1,747 checked profiler-MAP constructions (140 expected
invalid-Unicode exceptions). Its 115-record frozen fixture passes. This checks logging
and the Value boundary, not QueryProfiler or complete runtime execution.

Evidence: `runtime-native-check-18.jsonl`/`.log`, `runtime-foundation-lib-2-summary.json`,
`runtime-foundation-all-target-clippy-2.jsonl`, `file-logging-reference-2.tsv` and
`runtime-codegen-check.log` in the external `identifier-bytes-20261023` directory.
The first file-logging comparison exposed a harness mismatch between C++ prefixed and
Rust raw exception messages; the corrected oracle compares raw messages on both sides.
The original failed log remains preserved. The first broad test build was interrupted
after source changes so that final tests could be rebuilt against current code; it is
not counted as successful verification. Raw SQL parser ingress is required for
generated appender/COPY SQL; do not insert a lossy or checked byte-name-to-String adapter
to quiet these errors. Final shell, corpus, storage/WAL, C API, Python/selected clients
and documented speed-memory acceptance remain open. The uncached block-release policy
is unchanged.


The native Postgres-parser continuation (2026-10-24) now compiles all targets and
passes all-target Clippy with warnings denied. **39 signed release tests pass**, none
ignored. The scanner accepts native bytes, terminates input at NUL as C++ does, preserves
raw identifier/string/dollar-literal payloads and keyword casing, and leaves literal
validation to the later DuckDB Value-construction boundary. Parser exceptions retain
raw bytes and truncate at the exact 1,023-byte `vsnprintf` boundary, including a partial
UTF-8 sequence. Native Unicode-space stripping and tokenization APIs preserve other
bytes. The development tree-dump/comparison tools now use byte streams instead of lossy
text, and a differential mismatch makes the checker fail.

**19,708 exact C++ comparisons pass**: 9,854 SQL inputs in each of case-preserving and
ASCII-folding modes. Each mode has 9,487 identical accepted Postgres trees and 367
identical rejected errors/positions; the input set includes 1,238 invalid-UTF-8 queries,
NUL termination, every single input byte, malformed/long inputs and Unicode spaces.
The reference compiles the ten unchanged libpg_query sources of the frozen DuckDB 1.5.6
snapshot; it does not link a C++ engine into shipped Rust artifacts. The 130-record
frozen fixture passes. The native node generator matches the checked-in node types.
This proves the scanner/grammar/Postgres-tree component, **not full DuckDB AST, engine,
shell or client acceptance**.

The next gate is the DuckDB AST transformer's remaining text signatures (95 scoped
compile diagnostics). It must receive byte names, construct checked VARCHAR literals at
the actual C++ boundary, and preserve raw query/CREATE SQL/parameter identities before
appender/COPY SQL can use the native ingress. The earlier 700 pending full-AST byte
queries are still pending, not counted as passed by these Postgres-tree comparisons.
Evidence: `pg-native-checkpoint.json`, `pg-native-tests-3-summary.json`,
`pg-native-comparison-2.log`, `pg-native-fold-comparison-2.log`, `pg-native-clippy-3.jsonl`,
`pg-native-node-generator-check.log` and `pg-native-parser-integration-1.jsonl` in the
external `identifier-bytes-20261023` directory. The lower-memory block-release policy is
unchanged. Complete runtime/shell/corpus/storage/WAL/client/performance acceptance
remains open.


The full DuckDB AST/native SQL ingress continuation (2026-10-24) now compiles all
parser targets and passes scoped common/serde/AST/parser all-target Clippy with warnings
denied and codegen --check. **73 signed release tests pass**, none ignored; 205 new frozen
C++ records cover raw SELECT/error cases and raw statement codecs. The parser exposes
`parse_bytes`, `parse_bytes_with_options` and native statement splitting. Query source,
CREATE SQL, parameter identities/maps, identifiers, option maps/sets and additional
statement metadata retain bytes. Their serialized representation is still STRING;
case-insensitive sets now serialize native key types without writing byte lists.
The former checked name-to-text adapters in column constraints and generated PIVOT SQL
are removed. VARCHAR literals and generated constant expressions validate at the actual
C++ Value-construction boundary. Invalid raw ENUM labels retain bytes as C++'s
`StringVector::AddStringOrBlob` does, instead of taking the checked literal path.

**24,499 exact C++ full-AST comparisons pass**, with no byte inputs skipped:

- 8,776 SELECT binary-AST/original-SQL and processed error/metadata outcomes. All 700
  formerly pending byte-ingress inputs are included and pass.
- 15,723 statement outcomes across CREATE TABLE/NOT NULL, macro defaults, ALTER SET/
  RESET options, named parameters, COPY option keys, enum/index metadata and COPY file
  literals. There are 15,428 accepted statements (including 3,339 COPY cases) and 295
  matched errors. The test compares stored SQL bytes and binary metadata, separately
  captures COPY's nonserialized parsed-option keys, and verifies codec read/write round
  trips, including native case-insensitive sets.

The original saved SELECT reference used newly thrown C++ exception JSON, while Rust's
error is a caught ErrorData. Its 56 field-order differences have identical decoded
class/message/metadata. A fresh C++ oracle now compares ErrorData on both sides without
changing engine error formatting. That probe initially rendered four SELECTs after a
legacy serialization round trip; it now renders the original parsed tree on both sides.
All successful bytes/renderings remain identical to the original saved reference. Both
failed comparisons and the corrected evidence are preserved. An initial COPY fixture
used unsupported `option=value` syntax; the corrected matrix uses `option value` and
includes successful native COPY metadata, rather than counting those syntax errors as
COPY-key acceptance.

Evidence: `parser-native-checkpoint.json`, `parser-native-tests-2-summary.json`,
`parser-native-select-final.log`, `parser-native-statements-final.log`,
`parser-native-clippy-4.jsonl`, `parser-native-codegen-check.log` and
`parser-native-workspace-check-3.jsonl` in the external `identifier-bytes-20261023`
directory. The workspace now stops at 57 planner diagnostics; runtime/shell/C API,
public file/WAL journeys, final corpus, Python/selected clients and documented final
performance acceptance remain open. Native error highlighting and the previously
recorded fallible rendering boundaries also remain open. The uncached block-release
policy is unchanged.

Catalog/storage all-target Clippy also passes after checkpoint SQL initialization and
native unbound-index-type integration (`parser-native-catalog-clippy-1.jsonl`). This is
compilation/lint evidence; it does not replace a new cross-engine file/WAL journey.


The planner/native path continuation (2026-10-24) compiles planner/function SDK all
targets, passes all-target Clippy with warnings denied and codegen --check. COPY/export
logical metadata and SDK bind inputs now carry byte paths, format and option names;
SET/RESET/PREPARE/PRAGMA and bound-parameter identifiers remain bytes through the planner
and generated STRING properties. Secret storage/option identities also retain bytes.
This verifies interfaces and the scoped behaviors below, **not full COPY/export or
runtime execution**: downstream CSV/JSON/Parquet/execution/client integration is pending.

**59 signed release tests pass**; one existing exhaustive cast-grid test is ignored.
**28,021 new exact C++ comparisons pass**: 26,247 path expansion/generic/local
canonicalization outcomes, 27 extra path edge cases, and 1,747 bound-parameter binary
metadata/native text outcomes. The parameter test uses the actual BoundParameterMap
and additionally checks shared bind data for ASCII-case variants. There are 61 stable
path fixtures and 53 parameter fixtures in source. Access checks now preserve byte paths
and error messages instead of decoding them. The real SSD journey verifies Unicode
file read/write, raw-byte allow/deny/home expansion, and rejection of sibling-prefix,
parent-directory and symlink escapes from an allowed directory.

The first path matrix exposed four mismatches; matching C++ requires its all-empty
string-delimiter split fallback (e.g. generic "/" becomes "//"), unknown `file://` URL
absolute-path behavior and the C-string `realpath` boundary. The corrected matrix passes.
The original real-path test attempted invalid-UTF-8 directory names, which this APFS
volume rejects with errno 92. A signed C++ LocalFileSystem probe reports the same
Illegal byte sequence. The final test uses real Unicode names for OS I/O and preserves
raw bytes in permission checks/errors; it does not claim APFS can create invalid-UTF-8
filenames. The original failed test and C++ rejection evidence remain preserved.

Evidence: `planner-native-checkpoint.json`, `planner-native-tests-2-summary.json`,
`planner-native-path-tests-2-summary.json`, `filesystem-native-comparison-2.log`,
`filesystem-native-extra-comparison-2.log`, `planner-parameter-native-comparison-1.log`,
`planner-native-clippy-4.jsonl`, `planner-native-codegen-check.log` and
`planner-native-workspace-check-1.jsonl` in the external `identifier-bytes-20261023`
directory. The fresh workspace check stops at five CSV diagnostics. Other legacy
text-only filesystem APIs, non-Unix path semantics, full engine/shell/C API/storage/WAL,
final corpus, Python/selected clients and documented performance acceptance remain
open. The uncached lower-memory policy is unchanged.


The CSV native-path/file-list continuation (2026-10-24) compiles all CSV targets and
passes all-target common/function SDK/CSV Clippy with warnings denied plus codegen
--check. **Four signed CSV release tests pass**, none ignored (the crate has only four
release tests; this is not a corpus count). File lists, local glob expansion, CSV buffers,
scan/schema/sniffer/error/reject paths, file-filter callbacks and COPY initialization
now carry bytes. Native path checks are retained; directory entry names are read as OS
bytes rather than lossy text. Option-name dispatch compares bytes. Additional option
value/display codecs that still use text are not accepted by this checkpoint.

**13,577 exact C++ comparisons pass**: 10,075 hive parse/URL-decode/VARCHAR/BIGINT
outcomes (2,527 expected errors) and 3,502 glob lists/errors against a real SSD fixture
with Unicode filenames, nested directories and symlinks. Hive partition keys/values
retain bytes through parsing. URL decoding now preserves bytes until its actual C++
UTF-8 validation boundary and returns the same native error; typed hive values then use
the checked VARCHAR constructor. Auto-detection also propagates constructor failures
instead of silently replacing bytes. Seventy-five frozen C++ hive records pass in-source.

A real signed Rust journey expands a Unicode path, scans the CSV as INTEGER values
[42, 7], and writes BLOB bytes 610062ff through the native COPY path callback. A signed
C++ CLI journey produces the same values and file bytes on the SSD. This is direct CSV
SDK/file and COPY-function callback evidence, **not full runtime SQL COPY acceptance**.
The first glob comparison matched 3,501/3,502 patterns; the correction reproduces C++'s
NUL-terminated OS existence check while preserving the returned path's full bytes.
The failed log remains preserved. Other native OS read/write/NUL boundaries and the
pre-existing delimiter/value display conversions still require their own audit.

Evidence: `csv-native-path-checkpoint.json`, `csv-native-path-tests-1-summary.json`,
`csv-hive-native-final.log`, `csv-glob-native-final.log`, `csv-native-cpp-journey.json`,
`csv-native-path-clippy-4.jsonl`, `csv-native-path-codegen-check.log` and
`csv-native-path-workspace-check-1.jsonl` in the external `identifier-bytes-20261023`
directory. The fresh workspace check stops at 14 execution diagnostics; downstream
JSON/Parquet/runtime/client consumers remain unchecked by a complete build. Final
shell/corpus/C API/storage/WAL, Python/all selected clients and documented performance
acceptance remain open. The uncached block-release policy is unchanged.


The execution native COPY/export-path continuation (2026-10-24) compiles execution
all targets and passes all-target execution/planner/AST Clippy with warnings denied and
codegen --check. **36 signed release tests pass**, none ignored, covering window,
storage-backed operators, pipelines, native physical metadata, bound parameters,
filename fixtures and the new physical COPY/export journeys. Physical COPY state,
partition directories, returned filenames, cleanup and temporary-file rename retain
bytes. Filename-pattern literals and generated paths are native; numeric/UUID text
remains text. Generated pattern properties still write STRING bytes.

**12,229 exact C++ comparisons pass**: 10,482 filename-pattern metadata/deterministic
path cases and 1,747 export path SQL literals. UUID pattern metadata and UUID presence
are compared; random UUID values are not asserted byte-identical. There are 174 frozen
filename records plus 38 export-literal records in source. The first pattern comparison
exposed eight empty/slash-only parent path differences; C++ JoinPath always adds a
separator for a nonempty parent, even if it already ends in one. The correction matches
all cases. The initial failed log is preserved.

A signed Rust physical COPY pipeline writes a Unicode SSD path, renames its temporary
file, returns the final path/row count and produces BLOB bytes 610062ff. A signed C++
CLI journey confirms row count 1, the final returned filename, identical BLOB bytes and
no temporary file. A separate physical export source test writes exact schema.sql and
load.sql bytes with raw table/path metadata. C++'s export uses SQLString directly, so
raw export file literals are quoted without an artificial VARCHAR constructor check;
not-null column constants and returned COPY file Values retain their actual checked
construction boundary. This is physical-operator/component evidence, **not complete
runtime SQL COPY/EXPORT or storage/WAL acceptance**.

Evidence: `execution-native-path-checkpoint.json`, `execution-native-path-tests-2-summary.json`,
`filename-native-comparison-2.log`, `export-path-literal-comparison-1.log`,
`execution-native-copy-cpp.json`, `execution-native-path-clippy-4.jsonl`,
`execution-native-codegen-check.log` and `execution-native-path-workspace-check-1.jsonl`
in the external `identifier-bytes-20261023` directory. The latest workspace check
reaches Parquet and stops at 20 diagnostics. Parquet/JSON/runtime/client propagation,
full shell/corpus/C API/storage/WAL verification, all selected language bindings and
final documented performance acceptance remain open. Other OS/NUL/extension/value and
fallible rendering boundaries remain subject to their recorded audits. The uncached
lower-memory policy is unchanged.


The Parquet native-path continuation (2026-10-24) carries paths through reader/writer,
scan file lists, COPY callbacks, metadata-cache keys and shared external-cache entries
as bytes. Operator-log info retains bytes until its actual checked VARCHAR construction.
Parallel scan source parameters now retain bytes through execution profiling. Cache
sizing, enablement, eviction and table-block retention policies are unchanged.

**93 signed function SDK/Parquet release tests pass**, zero failures; three existing
tests remain ignored (Snappy tool/benchmark and full cast grid). The four new tests
cover all 256 cache-key bytes, a Unicode-path Parquet write/read/reopen with metadata
reuse, frozen native open error JSON/errno, and a real embedded-NUL OS read while
retaining the complete native key. **1,756 exact C++ open comparisons pass**: 1,747
byte-path cases plus nine real filesystem edges. The initial matrix failed all 1,747
cases because errno was omitted and NUL open behavior differed; its failed log remains.
The corrected native open copies thread-safe OS error bytes, includes errno and uses
C++'s C-string OS boundary while keeping full paths in diagnostics and cache keys.
Directory opens succeed like C++ (later read behavior is a separate boundary).

The signed cross-engine SSD journey creates a Unicode file/column in C++, reads it in
Rust, rewrites/reopens it in Rust, and reads the rewrite in C++. Both directions return
[42, 7] with column 値; the original SHA-256 is unchanged. This is Parquet SDK/file
component evidence, not final runtime SQL COPY/read_parquet or storage/WAL acceptance.
All-target execution/function SDK/Parquet Clippy with warnings denied and codegen
--check pass. A fresh workspace check passes these crates and stops at 23 JSON errors.

Evidence is in `parquet-native-path-checkpoint.json`, `parquet-native-path-tests-2-summary.json`,
`parquet-native-open-final.log`, `parquet-native-open-edge-final.log`,
`parquet-native-path-cross-engine.json`, `parquet-native-path-clippy-3.jsonl`,
`parquet-native-path-codegen-check-2.log` and `parquet-native-path-workspace-check-2.jsonl`
in the external `identifier-bytes-20261023` directory. Other writer/create/NUL and
cache-lookup OS boundaries, non-Unix paths, and pre-existing lossy metadata/statistics
rendering remain open (`parquet-native-path-followup.md`). JSON/runtime/client integration,
full shell/corpus/C API/storage/WAL, Python and every selected client, and documented
final performance acceptance remain open. The lower-memory baseline is preserved.


The JSON native-path continuation (2026-10-24) carries reader paths, pipe-content map
keys, COPY FROM inputs/options, scan metadata and hive keys as bytes. Reader and COPY
option names compare ASCII bytes directly; unknown keys and NULL diagnostics preserve
native bytes rather than using an ASCII/UTF-8 adapter. Filename values use the actual
checked VARCHAR construction boundary. OS file/metadata calls convert native path
bytes at their filesystem boundary; no lossy path decoder was added.

**15 signed JSON release tests pass**, zero failures, with three existing timing probes
ignored. The public read_ndjson bind/scan journey reads a Unicode SSD path and column,
returns exact filename/hive metadata and values [42, 7], then exercises the JSON COPY
FROM bind/scan callbacks with INTEGER target data. A signed C++ CLI journey confirms
matching filename/hive rows and COPY FROM values on the SSD. This is JSON SDK/component
verification, not complete runtime SQL or shell acceptance.

**12,262 exact C++ comparisons pass**: 7,021 new actual JSONMultiFileInfo reader/COPY
option state/error outcomes and 5,241 rerun native COPY projection cases. Option cases
cover raw unknown keys, NULL errors, ASCII case variants and COPY argument cardinality;
82 frozen option records are in source. All-target JSON Clippy with warnings denied
and codegen --check pass. A fresh workspace check passes JSON and reaches 48
autocomplete parser-transformer/AST diagnostics.

Evidence: `json-native-path-checkpoint.json`, `json-native-path-tests-2-summary.json`,
`json-native-option-comparison-1.log`, `json-native-copy-plan-comparison-1.log`,
`json-native-cpp-journey.json`, `json-native-path-clippy-2.jsonl`,
`json-native-path-codegen-check.log` and `json-native-path-workspace-check-1.jsonl`
in the external `identifier-bytes-20261023` directory. Embedded-NUL/errno OS handling,
FIFO and parallel-range journeys, non-Unix paths, and option-value text boundaries
remain open (`json-native-path-followup.md`). Autocomplete/runtime/client integration,
full shell/corpus/C API/storage/WAL, Python/all selected clients and final documented
performance acceptance remain open. No table caching or memory policy changed.


The PEG/autocomplete integration continuation (2026-10-24) carries new AST option maps,
COPY/EXPORT format/path fields, parameter metadata, attach/secret/index/setting names
and statement source through native carriers. Existing Unicode token strings encode
into byte names at the current PEG text ingress; this does not establish raw-byte PEG
tokenization. Statement source slicing no longer uses a lossy UTF-8 decoder. PIVOT
statement helper output stays native rather than decoding generated SQL into text.
COPY path/format Values and partition column references use native value bytes.

**18 signed autocomplete release tests pass**, zero failures and none ignored. **188
direct C++ PEG comparisons pass**: 182 actual PEGTransformerFactory AST binary/source
records and six exact caught-error JSON outcomes. Cases cover Unicode/control names,
COPY paths/format inference/options, table/index option maps, SELECT parameters and
CTEs; errors cover duplicate/invalid options and mixed named/positional parameters.
All 188 records are frozen as regression fixtures. All-target autocomplete Clippy with
warnings denied and codegen --check pass.

The fresh integration check also exposed one empty SQL-source carrier in each of
TPC-H/TPC-DS, now encoded as an empty ByteString. Generator all-target Clippy and both signed native-name scale-zero tests pass.
These are schema/identity journeys, not full data or performance acceptance.
A new workspace check passes these components and stops at 125 runtime diagnostics;
this is not complete source/build or public binary acceptance.

Evidence: `peg-native-checkpoint.json`, `peg-native-tests-2-summary.json`,
`peg-native-statement-comparison-1.log`, `peg-native-error-comparison-1.log`,
`peg-native-clippy-3.jsonl`, `peg-native-codegen-check.log`,
`peg-native-generator-clippy-1.jsonl` and `peg-native-workspace-check-3.jsonl`
in the external `identifier-bytes-20261023` directory. Raw-byte PEG ingress, known
fallible-rendering boundaries, complete runtime/shell/corpus/C API/storage/WAL, Python
and all selected clients, and final documented performance acceptance remain open.
The uncached lower-memory policy is unchanged; `peg-native-followup.md` records limits.


The runtime continuation's native error-context foundation (2026-10-24) exposes
`Error::with_query_context_bytes` and preserves original query bytes in displayed
source lines. Valid UTF-8 uses the frozen utf8proc grapheme/width data already used by
the native tree renderer. C++'s invalid-UTF-8 line fallback assigns width one to every
byte; no replacement decoder is used. The text Error API delegates to this boundary.

**32 signed common release tests pass**, zero failures and none ignored. **6,348 exact
C++ QueryErrorContext outputs pass**, covering byte names, invalid UTF-8, multiline
CR/LF, long-line truncation, combining marks, emoji and positions inside sequences.
There are 104 frozen regression records. Common all-target Clippy with warnings denied
and codegen --check pass. This verifies the common boundary, not full runtime errors.

The same continuation began native runtime attachment option metadata, appender SQL
construction and read_file paths/hive metadata. Those runtime edits are **unverified**:
the library still stops at 99 integration diagnostics (query lifecycle/profiling,
settings, attachment paths, read_duckdb and secrets). Do not treat reduced diagnostic
counts as engine acceptance. Attached-info option maps now hold bytes; appender-generated
SQL parses bytes and its failure prefix retains native error bytes. Full public SQL,
appender/file, storage/WAL, shell, clients and performance remain pending.

Evidence: `query-context-native-checkpoint.json`, `query-context-native-tests-1-summary.json`,
`query-context-native-comparison-1.log`, `query-context-native-clippy-2.jsonl`,
`query-context-native-codegen-check.log` and `runtime-native-integration-check-4.jsonl`
in the external `identifier-bytes-20261023` directory. The native query-context API is
not yet wired throughout runtime execution; finish lifecycle/profiler consumers and
verify actual SQL errors before claiming parity. The lower-memory policy is unchanged.


The native query-state continuation (2026-10-24) stores exact bytes in FunctionClient,
prepared queries and ActiveQuery. Parsed-statement execution, logging and error context
accept byte inputs while preserving existing text callers. The profiler retains native
source text and validates QUERY_NAME when C++ assigns its VARCHAR metric; its query
header remains bytes. These central runtime edits remain **unverified end-to-end**:
the runtime library does not build yet. The fresh check reports 91 diagnostics in
database/settings/attachment handling, read_duckdb and secrets; the changed query-state,
active-query and profiler modules report no type diagnostics in that check.

The shared current_query scalar now returns a checked VARCHAR at C++'s actual Value
construction. **41 signed builtin-function library tests pass**, none ignored, including
exact query storage/ID and real scalar-kernel cases. **1,747 C++ SQL outcomes pass**
through Rust's raw parser, FunctionClient storage and the actual bound current_query
kernel; 1,602 return query bytes and 145 produce expected parser/value errors. The 72
frozen regression records pass in an additional signed test. This is parser/SDK/kernel
component evidence, not a complete Connection::query or shell journey.

The initial oracle matched 1,737/1,747. Ten NUL-comment errors differed because the
comparison omitted ClientContext's parse-error rethrow and Error::rethrown did not
sanitize NUL introduced by attached query context. The corrected rethrow reproduces
C++'s next ErrorData catch; all original cases pass and the failed log remains. Direct
QueryErrorContext formatting continues to preserve raw bytes before rethrow. A fresh
33-test signed common suite passes (none ignored), including a NUL rethrow regression.
Common/function SDK/builtins all-target Clippy and codegen --check pass.

Evidence: `runtime-query-native-checkpoint.json`, `runtime-query-sdk-tests-1-summary.json`,
`runtime-query-regression-tests-1-summary.json`, `runtime-current-query-comparison-3.log`,
`runtime-query-final-clippy-1.jsonl` and `runtime-query-native-check-4.jsonl` in the
external `identifier-bytes-20261023` directory. Full runtime query/profiler, settings,
attachments/read_duckdb/secrets, shell/corpus/C API/storage/WAL, Python/all selected
clients and documented final performance acceptance remain open. The uncached
lower-memory policy is unchanged. `runtime-query-followup.md` records scope limits.


The runtime settings/pragma continuation (2026-10-24) resolves built-in option names
from native bytes to canonical static table entries, preserving unknown names and
candidate/error metadata as bytes. Extension option matching and pragma dispatch use
byte comparisons. COPY DATABASE generated SQL and IMPORT DATABASE source/path assembly
now remain native; secret option/storage dispatch also retains native names. These
runtime edits are **not functionally accepted**: the library still fails compilation.
The latest scoped check reports 14 diagnostics, all at attachment/read_duckdb path
interfaces; no byte-name-to-text adapter was added to bypass those storage boundaries.

The separately buildable native extension-install hint helper is verified against
**13,976 actual C++ ExtensionHelper outputs**, covering 1,747 native base strings,
autoload/autoinstall combinations, and httpfs/custom extension hints. The helper keeps
the base bytes and appends the same static hint as the existing text function. SDK
all-target Clippy passes. Native-byte/text-contract and frozen C++ fixture tests are
recorded in the signed test evidence. This helper evidence does not validate runtime
SET/RESET errors, settings aliases, locking/scopes, pragma SQL or secret operations;
those require a built engine and actual SQL journeys.

Evidence: `settings-native-checkpoint.json`, `settings-hint-native-comparison-1.log`,
`settings-native-sdk-tests-2-summary.json`, `settings-native-sdk-clippy-2.jsonl` and
`settings-native-check-8.jsonl` in the external `identifier-bytes-20261023` directory.
Continue with a coherent attachment/storage/WAL/native path migration, then run runtime
settings/pragma/secret/import/COPY journeys. Full shell/corpus/C API/storage/WAL,
Python/all selected clients and final performance acceptance remain open. The accepted
uncached lower-memory policy is unchanged.


The native attachment/storage path continuation (2026-10-24) now compiles rustdb --lib.
Database/AttachedDatabase/AttachedInfo, read_duckdb paths/file lists and storage block
handles/WAL paths retain bytes. Common native_file_path converts at OS calls, using
C-string NUL termination while retaining full bytes in carriers and diagnostics.
Runtime WAL replay, checkpoint-WAL merge/removal/rename and storage opens use native
OS paths. BufferManager's temporary directory retains PathBuf internally; settings
metadata builds a checked VARCHAR at its Value boundary. No table-cache retention or
memory policy changed.

**Eight signed release persistence tests pass**, none ignored: checkpoint restart,
committed WAL replay, torn-tail recovery, checkpoint WAL cleanup, persisted indexes,
bulk insert/rollback and checkpoint-skipping commits. **One additional signed runtime
journey passes** with Unicode SSD database/attachment/column names, ATTACH/DETACH,
checkpoint, disabled checkpoint-on-close and WAL reopen. Reopened sums are [58, 49].
A signed C++ CLI performs the same journey and reports [58, 49], with a nonempty WAL
before replay. These are separate engine journeys; this checkpoint does **not** yet
prove final cross-engine checkpoint/WAL replay compatibility or raw-name public SQL.

Storage and buffer library Clippy with warnings denied and codegen --check pass.
Runtime library Clippy reports 71 diagnostics, predominantly borrow/conversion cleanup,
items-after-statements and existing fallible rendering expectations; keep it open.
The workspace all-target check stops at one benchmark Rust adapter column-name String
mismatch. New runtime source changes made after the signed journeys are documentation
and redundant PathBuf conversion cleanup; exact test snapshots/logs remain preserved.

Evidence: `attachment-native-checkpoint.json`, `attachment-native-persistence-tests-1-summary.json`,
`attachment-native-path-tests-1-summary.json`, `attachment-native-cpp-journey.json`,
`attachment-native-storage-clippy-1.jsonl`, `attachment-native-clippy-diagnostics-3.json`,
`attachment-path-native-check-4.jsonl` and `attachment-path-native-workspace-check-1.jsonl`
in the external `identifier-bytes-20261023` directory. Finish runtime lint/all-target
integration and actual cross-engine storage/WAL, then verify settings/pragma/secrets,
shell/corpus/C API, Python/all selected clients and final documented performance.
Existing temp-file display/non-Unix path behavior remains subject to audit.


The runtime lint/interchange continuation (2026-10-24) closes all 71 prior runtime
library Clippy diagnostics. Most changes are exact compiler-suggested borrow/conversion
cleanup. Profiler private map entries now store text after the existing checked VARCHAR
construction at CreateTree/MergeOperatorExtraInfo, removing unchecked Value downcasts
without moving that validation boundary. Runtime library Clippy with warnings denied
and codegen --check pass. Full runtime all-target tests and remaining fallible rendering
behavior still need verification.

A signed Rust runtime probe and signed C++ CLI verify **two-way checkpoint plus WAL
replay** on Unicode SSD database paths: C++ creates a checkpoint and nonempty WAL read
by Rust; Rust creates the same scenario read by C++. Both readers report count 3,
sum 58 and two non-NULL strings. Each source WAL is 131 bytes; the two WAL SHA-256 hashes
are identical for this scenario. Verification uses copies; original database/WAL hashes
are unchanged. This is actual cross-engine evidence for this data/schema/transaction,
not a complete storage/WAL compatibility matrix. Integer/VARCHAR/NULL and Unicode
column/data names are covered; other types/recovery/encryption scenarios remain open.

Benchmark result-column names now retain bytes in Rust and C++ adapters; its JSON
metadata keeps strings for UTF-8 and uses byte arrays otherwise. SLT result names are
native on both backends. Human-readable previews are presentation text. Existing
benchmark result_query / SLT result-file SQL still require UTF-8 names and explicitly
report unsupported metadata instead of replacing bytes. These harness limits remain
open for native-name coverage. **42 signed benchmark unit tests pass**, none ignored,
including the name metadata regression. Benchmark/SLT all-target Clippy passes. The
fresh workspace check advances to eight shell integration diagnostics.

Evidence: `runtime-lint-checkpoint.json`, `runtime-storage-interop.json`, source/copy
files under `runtime-storage-interop/`, `runtime-lint-bench-tests-1-summary.json`,
`runtime-lint-final-lib-1.jsonl`, `runtime-lint-harness-check-1.jsonl`,
`runtime-lint-codegen-check.log` and `runtime-lint-workspace-check-3.jsonl` in the external
`identifier-bytes-20261023` directory. Continue shell/C API/all-target integration,
actual runtime settings/pragma/secret/import/COPY verification, expanded cross-engine
storage/WAL, corpus, Python/all selected clients and documented final speed-memory
acceptance. The uncached lower-memory policy is unchanged.


The shell native-byte continuation (2026-10-24) is **work in progress and does not build**.
The latest shell/all-target check reports three errors: BoxRendererState still expects
String column names, and describe_table_name still returns String for two native AST
names. This count only describes compiler progress; no shell behavioral acceptance is
established. The prior verified runtime/harness/interchange checkpoint remains scoped
to its source snapshot.

Current unverified edits carry result metadata names as ByteString, add PrintStream
native-byte output for the existing layout renderers, use byte-oriented metadata
HTML/C/CSV/JSON/SQL escapes, and preserve native shell log bytes with ASCII-folded
deduplication. Prompt settings/generated text/truncation/highlight output, linenoise
prompt ingress and progress text also now retain bytes. Remaining duckbox internal
name/value/layout carriers and DESCRIBE/table metadata must be migrated coherently;
do not decode names into String to satisfy the compiler. Existing result VARCHAR
conversion still uses a lossy string_column helper and remains an open audit.

Evidence: `shell-native-wip-checkpoint.json`, `shell-native-integration-check-8.jsonl`
and `shell-native-integration-diagnostics-8.json` in external `identifier-bytes-20261023`.
Next finish duckbox/DESCRIBE native carriers, compile all shell targets, run signed shell
unit tests and exact C++ raw-output/PTY comparisons, then repeat workspace all-target
checking to expose C API/client integration. Prompt/progress/native escape behavior
also requires C++ differential verification. The full goal and accepted lower-memory
policy remain unchanged; no final shell/corpus/storage/WAL/client/performance gate is
closed by this work-in-progress checkpoint.


The shell native metadata continuation (2026-10-24) compiles all shell targets and
passes all-target Clippy with warnings denied. Duckbox layout/header names and
DESCRIBE/table/database/schema names retain bytes. Streaming metadata output uses
native PrintStream/highlight APIs; HTML/C/CSV/JSON/SQL metadata escaping is byte-based.
Prompt setting/generated/truncated text, progress, linenoise prompt ingress and log
messages retain bytes. Pivoted names validate where C++ row_chunk.SetValue constructs
VARCHARs. The batch SQL handoff no longer decodes lossily: new runtime byte statement
APIs preserve default-parser input and processed error context.

**15 signed current-source shell unit tests pass**, none ignored (development-profile
executables). The earlier 14 signed release tests passed before native SQL ingress and
the width correction. A signed release CLI passes **154/154 native metadata cases**
across list/csv/json/jsonlines/html/quote/tcl/line/column/table/markdown/box/duckbox/insert,
using Unicode, controls, invalid UTF-8 and long names. Only C++ platform internal stack
traces are normalized; raw output is retained. The first matrix passed 144/154: aligned
and duckbox widths used forgiving invalid-byte handling instead of C++ native utf8proc
traversal. The corrected analysis propagates the native decoder error and matches the
truncated-sequence width. Failed outputs are preserved.

The **768-case canonical differential suite reports zero unexpected differences**,
with three existing known differences (internal stack traces, database OIDs and invalid
argv decoding), on the native SQL-ingress release binary before the final width change.
It includes PTY cases. Do not describe that as a full final-source rerun. The corrected
binary has the new 154-case matrix and current-source signed unit tests; repeat the
complete shell acceptance suite, including upstream pytest, at final integration.

Runtime strict PEG invalid-byte ingress explicitly remains NotImplemented; text PEG
queries retain existing override behavior, while default raw parsing preserves bytes.
Existing result-value string_column/numeric/JSON width text helpers, infallible legacy
width callers and invalid-byte argv still require audit. This checkpoint is metadata
and tested ingress behavior, not complete shell parity.

Bindcheck's Rust result-name carrier/cache now preserves bytes (UTF-8 remains strings
in JSON, other names use byte arrays); its text CLI oracle path still requires audit.
Bindcheck all-target Clippy and codegen --check pass. The fresh workspace check advances
to 29 C API diagnostics in logging, Arrow, COPY and other native consumers.

Evidence: `shell-native-checkpoint.json`, `shell-native-tests-2-summary.json`,
`shell-native-cli-diff-summary-2.json`, `shell-native-canonical-diff.log`,
`shell-native-clippy-6.jsonl`, `shell-native-bindcheck-clippy-1.jsonl`,
`shell-native-codegen-check.log` and `shell-native-final-workspace-check-2.jsonl` in the
external `identifier-bytes-20261023` directory. Continue C API/client integration,
remaining raw codecs/PEG ingress, expanded storage/WAL/corpus, Python/all selected
clients and final documented speed-memory acceptance. The uncached lower-memory
policy is unchanged.


The C API native-byte continuation (2026-10-24) closes its 29 integration diagnostics
and reaches successful workspace/all-target compilation. Query/extract/prepare paths
now read native C-string bytes. Prepared parameter maps/lookup/execution retain native
identities through generic runtime maps. Logical STRUCT/UNION/ENUM member names and
aliases, C result names, Arrow schema names, table bind output names, COPY paths/options,
replacement callback names and callback log messages retain bytes. Instance cache keys
retain OS path bytes and use ASCII case folding. Remaining text-only definition/option
interfaces, Arrow view/query paths and value codecs remain audited separately.

The signed rebuilt library passes **all 30 upstream C API harness files**, with **155
test cases and 795,736 assertions**; the harness keeps its documented C++-internal
exclusions. This run used library build1. The prepared suite rerun on build2 passes
1,235 assertions in nine cases. **8,735 actual C++ C ABI outcomes match** (1,747 inputs
for each of five surfaces: query/result names, prepare/parameter/bind/execute, STRUCT,
UNION and alias). The first completed comparison passed 8,601/8,735; all 134 failures
were raw prepared names hitting BoundExpression bookkeeping's bound.to_string(). Native
bound.to_bytes() fixes the panic. **Two signed planner release regression tests pass**.

A maintained duckdb.h C test (`crates/capi/tests/native_names.c`) passes seven name
journeys on both libraries, including native result names from a real table-function
callback. Three invalid native path cases are rejected by both C APIs, preserve original
name bytes in errors, and never return an in-memory handle. C API/planner all-target
Clippy with warnings denied and codegen --check pass. Export inspection finds all
**546 duckdb.h functions** and no missing header exports; C++ has 547 duckdb_ exports,
with **duckdb_adbc_init** the missing additional entry point at that recorded snapshot (implemented in the later metadata checkpoint).

The original raw ABI collector aborted in C++ while reading an invalid ENUM dictionary
value: duckdb_enum_dictionary_value materializes a Value and throws across the C ABI.
An exception-aware C++ probe plus Rust accessor audit records **1,613 matching returns
and 134 real C++-exception/Rust-return gaps**. These are not counted as passing ABI
outcomes. Their public failure semantics remain unresolved; do not hide them or label
complete parity. The Python ctypes scripts are verification tools, not Python bindings.

Evidence: `capi-native-checkpoint.json`, `capi-native-upstream-summary.json`,
`capi-native-abi-summary.json`, `capi-native-enum-audit.json`, `capi-native-exports.json`,
`capi-native-invalid-paths-summary.json`, `capi-native-planner-tests-1-summary.json`,
`capi-native-final-clippy-1.jsonl`, `capi-native-codegen-check.log` and
`capi-native-final-workspace-check-7.jsonl` in the external `identifier-bytes-20261023`
directory. Existing text-only callback definitions, remaining Arrow/query/value codecs,
ENUM failure semantics, ADBC, strict PEG native ingress, final unit/corpus/shell/storage/WAL,
Python/all selected clients and final performance acceptance remain open. No table
caching or memory policy changed.


The ADBC core continuation (2026-10-24) adds the frozen C structures with 13 measured
layout assertions, database/connection lifecycle and options, manual transactions,
statement prepare/execute/result/parameter schemas, streaming-result ownership and
Arrow batch/stream parameter binding. Preparing/executing another query or releasing
the connection buffers unread result batches; starting a query does not eagerly buffer
its results. Single-batch streams transfer input ownership and return independent schema
copies, preserving nested fields, dictionaries, native names and binary metadata.

**Seven signed C API library tests pass**, none ignored (six new ADBC regressions plus
one existing Arrow test), using real engine queries. Three corresponding C++ public-ABI
journey families pass. The legacy error-prefix canary, short option buffers, manual
commit/rollback, parameter row limit and exact interrupt-message classification are
covered. C API all-target Clippy with warnings denied, complete workspace/all-target
checking and codegen checking pass. These are scoped internal-core results.

**duckdb_adbc_init remains unexported.** Initializer wiring/version-prefix bounds,
ingestion and statement options, connection metadata/statistics, 1.1 error details,
deferred stream errors, upstream ADBC tests and actual driver-manager loading are still
required. No generic unsupported callbacks were published to claim a complete driver.
Python and every selected client, the existing ENUM failure gap and final full
unit/corpus/shell/storage/WAL/performance acceptance remain open. The uncached memory
policy is unchanged. Evidence and source hashes: `adbc-core-checkpoint.json` in external
`identifier-bytes-20261023`; detailed inventory: `docs/adbc.md`.


The ADBC ingestion/native appender continuation (2026-10-24) stages typed options and
all four ingestion modes, including temporary targets, explicit catalog/schema lookup,
affected rows and representative frozen error cases. Native table description, appender
lookup, selected columns and query-appender SQL/name/CTE input retain bytes. The first
expanded public C test exposed a join-statistics expression.to_string() panic during
selected-column flush; native to_bytes comparison fixes it. The optimizer regression,
seven-name C journey on both libraries and 1,747 complete C ABI journeys pass. Every
matrix journey exercises native catalog/schema/table/column/CTE names and returns 49.

Eleven signed library tests pass, none ignored (nine C API, two optimizer), and five
C++ driver-ABI journey families pass. Three affected upstream C API files pass 16 test
cases and 615,708 assertions against the rebuilt release library. C API/runtime/optimizer
all-target Clippy with warnings denied and workspace/all-target compilation pass. A C++
option comparison caught Mac negative-NaN spelling; native %f formatting now matches
C++ for both NaN signs, finite numbers, infinity and negative zero. Failed comparison
and appender backtrace evidence are preserved. No failing assertion was removed.

ADBC metadata/statistics, 1.1 error details/deferred stream errors, catalog/schema option
Value boundaries, initializer prefix bounds, full upstream ADBC and actual driver-manager
loading remain open; duckdb_adbc_init is still unexported. Python/all selected clients,
ENUM failure semantics and final unit/corpus/shell/storage/WAL/performance acceptance
remain required. No table caching/memory policy changed. Evidence and exact source
hashes are in `adbc-ingest-checkpoint.json`, with scope in `docs/adbc.md`.


The ADBC metadata/initializer continuation (2026-10-24) implements object/table metadata,
dense-union info/statistics, typed rich errors and ErrorFromArrayStream. Catalog/schema
option reads now use C++'s query/Value boundary. All 54 frozen callbacks are wired into
versioned duckdb_adbc_init; legacy prefix canaries/guard pages and untouched slots pass.
Export sets now match 547/547. Thirteen signed C API tests, seven C++-ABI probe families
on both libraries, and both cancellation/recovery cases pass. Actual Apache Python ADBC
driver-manager loading passes 12 journey comparisons; 13 direct metadata/PyArrow cases
also match with full Arrow validation. The C++ reference build reports v0.0.1 and Rust
v1.5.6: each info version matches its own core callback, and only those verified labels
are normalized. Initial failed version/deferred-error comparisons remain preserved.

The current rebuilt library passes all 30 upstream C API files (155 cases, 814,544
assertions in this run, unchanged private-C++ exclusions). C API all-target Clippy and
workspace/all-target compilation pass. The 64-case upstream ADBC suite is still unrun;
complete its helper adaptation through Rust/native public APIs without a C++ engine
fallback. Broaden native metadata/errors and lifetime/type/interchange cases, then
continue Python-first and all selected clients. ADBC client probes are not the DuckDB
Python package. Final full engine/corpus/shell/storage/WAL/performance acceptance and
the existing ENUM gap remain open. No table caching policy changed. Exact evidence and
source hashes are in external `adbc-metadata-checkpoint.json`; scope: `docs/adbc.md`.


The full upstream ADBC continuation (2026-10-24) passes all 64 frozen cases/3,040
assertions on both Rust and C++, preserving every case/assertion. The opt-in native
Rust test bridge replaces private C++ helper access; the Rust runner never links the
reference bridge/engine. Image checks enforce one selected DuckDB library. Shipping
artifacts are rebuilt without the bridge and retain 547 duckdb_ exports, no test hooks.
Reference calibration fixed helper stream-identity and nested getter issues. The first
Rust run's sole failing case exposed missing SELECT-containing MULTI preprocessing;
PIVOT enum creation and SELECT now unpack before extracted-statement preparation. The
public C regression passes both libraries; two signed runtime regressions pass, including
native byte identifiers. Non-SELECT multi/pragma/transaction-policy extraction remains
an explicit broader audit, not silently declared complete.

The restored shipping library passes 30 upstream C API files (155 cases/706,158
assertions in this run), 13 signed C API unit tests, default/test-feature Clippy and
workspace/all-target checking. No source assertion was removed; failure logs and the
first test library are retained. Evidence/source hashes: `adbc-upstream-checkpoint.json`
on the SSD; runner documentation: `scripts/adbc-tests/README.md`. Continue Python-first
and all selected language clients, broad core/native/fuzz/lifetime coverage, ENUM/PEG/
codec gaps and final engine/corpus/shell/storage/WAL/performance acceptance. No table
caching or accepted speed-memory policy changed.


The Python foundation continuation (2026-10-24) pins duckdb-python v1.5.6 commit
802a345bb627435d2a05d63cb74454fa98b60b5e and inventories 414 native stub surfaces plus
frontend/value/packaging requirements. A real Rust/PyO3 extension and installable duckdb
wheel now exist, linking the Rust runtime directly. The initial signed release wheel
passes seven upstream tests; current signed development-wheel evidence is 42/42 strict
C++ outcomes and 51 passed/50 failed in the full selected 101-test cohort (C++101/101,
no skips). No failed test was removed or normalized. Fixes include >128-bit integer
errors, negative timestamp-ns truncation, aware TIME, session/pytz timezone conversion,
parent/cursor closure, default recreation and fetch-after-close errors. Database pooling
uses weak references and cursor fetching remains chunk-based; no table cache introduced.

All failures and initial artifacts are preserved. The next Python priority is relations
(sql/query/table and lifetimes), which currently block many dictionary tests, followed
by host interchange, expressions, UDF/replacement scans and the rest of the inventory.
Full upstream Python, remaining clients and final core/native/storage/WAL/shell/corpus/
performance acceptance remain open. Evidence: python-bindings-20261024/
python-foundation-checkpoint.json; implementation scope: docs/python-bindings.md.
