# Scalar byte-name release verification

2026-10-25; goal active. This checkpoint does not complete the Rust port.

Release build 2 completed in 6m56s. Signed immutable images and SHA-256 identities:
`.context/artifacts/scalar-release-20261025/signed-images-2.json`.

- Upstream C API: 30 files, 155 cases, 726,562 assertions pass; existing harness
  exclusions remain. This is not complete C API semantic acceptance.
- Upstream shell: C++ and Rust each 392 pass / three skips, identical 395 test IDs.
  Only the pytest module-root prefix is normalized for identity comparison. Both
  use the documented SSD lexical-path fixture; test bodies/assertions are unchanged.
- Shell differential: zero unexpected differences / 768 cases, including 46 PTY.
  Three known gaps remain open: native internal-error stack traces, database OIDs,
  and invalid-UTF8 command-line arguments. They are not silently accepted as parity.

Fresh complete corpus: `full-corpus-1/manifest.json`, `run.log`, per-file worker logs
and eventual `rust-run.jsonl` under the same external artifact task. It runs only one
worker, with normal engine threading and immutable signed images. At 2,984 completed
files, three unexpected ratchet failures are recorded:

| Test | Diagnosis / correction required |
|---|---|
| `test/sql/json/issues/internal_issue5288.test` | Restore ASCII-case-insensitive compression-name lookup while preserving raw-byte diagnostics; audit the adjacent secret-persistence enum conversion |
| `test/sql/logging/logging_csv.test` | Preserve bounded small-file allocations and original file-size boundary while restoring C++'s final zero-length read/log event |
| `test/sql/logging/logging_types.test` | Same CSV EOF-read behavior |

The partial run also has documented extension-loading and lean-memory divergences.
Let the run finish to capture every result before compiling corrections. Do not merge
historical verdicts into this new run. Compare its exact coverage with the frozen C++
reference and enforce all 4,855 ratchet entries. Record correction validation separately
from this pre-correction run. Diagnosis: `full-corpus-1/regression-diagnosis.md`.

Detailed current receipts: `release-verification-2.json`, `capi-summary-2.json`,
`shell-release-comparison-2.json`, `shell-diff-summary-1.json` and their raw logs/XML.
Remaining Python/environment/API, all selected clients, broad storage/WAL/version/
encryption/recovery, and final signed-release speed-memory acceptance remain open.
The historical uncached performance baseline remains the control; no table cache
is introduced or authorized by this checkpoint.

## Terminal corpus result and follow-up

The fresh run finished with 4,774 pass / 21 fail / 75 skip, exactly 4,870 unique files
matching the frozen C++ reference. Ratchet: seven regressions, zero newly passing.
Its process exit was zero; that does not make its failed files an acceptance pass.
`full-corpus-1/terminal-summary.json`, `dashboard.md` and `ratchet.log` retain the result.

Five failed files share the compression-name and CSV EOF causes above, including
`storage/compression/test_using_compression.test` and `zstd/zstd_giant_string.test_slow`.
A sixth failure is `types/type/test_make_get_type.test`: catalog type binding incorrectly
uses the default binder's expression-derived names. C++ default binding uses GetName;
catalog/make_type binding uses explicit GetAlias. Preserve the former and add an
explicit alias-based route for the latter, retaining native bytes and mixed-name errors.

The seventh regression is a `disk_space 40gb` skip in
`aggregate/distinct/grouped/distinct_grouping_tpch.test_slow`. Both harnesses probe
`.`. The current source working directory is on the internal volume (~25 GiB free),
although test output is linked to the external SSD (~123 GiB free). Repeat using a
dedicated external working root that links the same frozen source/test/data entries,
with its own actual external test directory. Do not change the requirement or clean
unrelated data. Preserve this first run as-is; use new outputs for the rerun.

The first external symlink-only test view produced absolute canonical test IDs, so
C++ matched no cases. Its setup failures are retained in `cpp-controls-3/`. The second
view copies the 47 MB test tree, with all 4,870 test SHA-256 values checked against the
frozen source, while linking other unchanged inputs. Six C++ controls then pass, but
its disk-space control still skips because C++ unittest resets cwd to its compiled
source root unless `--test-dir` is supplied. Correct the Slt unittest adapter to pass
its requested root through that existing upstream option. Retain `cpp-controls-4/`;
use separately labeled direct C++ controls/new harness results rather than rewriting it.

## Correction source and component verification

All six functional failures now have corrections prepared: byte-preserving ASCII enum
lookup; bounded CSV EOF reads without redundant plain-UTF8 read-ahead; and separate
explicit-alias catalog type binding while preserving expression-derived default binding.
The Slt unittest adapter now forwards `--test-dir` to honor its selected source root.

Signed release component tests pass: parser20, types104, growing-CSV boundary1 and
FIFO1 (126 total). All seven direct frozen C++ controls pass with the actual external
test directory, including the 40 GB requirement. This verifies the control setup;
the corrected Rust engine has not yet passed those end-to-end cases. Release build3
is in progress. After it completes, sign and snapshot its binaries, run the focused
Rust/control cases, and repeat broader validation with new output paths.

Evidence: `correction-checkpoint-3.json`, `native-tests-3/`, `cpp-controls-5/`,
`source-corrections-4.json` (seven hashed source overlays on the preserved full source
archive), and `external-source-root-2.json` (all4,870 test hashes checked). Preserve the
first complete corpus and rejected control setups; do not merge them into acceptance.

## Exact C API library identity

The C API test script currently re-signs the default target library even with
`--no-build`. The Rust dylib also has an absolute install name pointing at the mutable
`target/release/deps/libduckdb.dylib`; linking with a snapshot directory alone does not
pin the loaded image. Earlier behavioral results remain recorded, but final image
verification must select and prove the actual library path and SHA.

Add an explicit `RUSTDB_CAPI_LIBRARY_DIR` override requiring `--no-build`. It must use
the already-signed selected image without re-signing it, and on macOS rewrite only
the newly built test executables' dependency to that exact image before signing the
executables. A separate `RUSTDB_CAPI_TEST_BIN_DIR` preserves fresh executable/log output
while reusing the existing object cache. Keep the frozen test bodies and harness
exclusions unchanged. Portable install names for eventual native client packages are
a separate packaging gate; this test adapter must not silently mutate preserved dylibs.

## Corrected release3 checks

Release3 completed in11m20s; all three images are signed and preserved in
`signed-images-3.json`. All seven affected Rust files pass, and all seven C++ controls
pass through the rebuilt unittest adapter with no skips (`focused-rust-3/`,
`cpp-controls-6/`). These are focused results, not a replacement for the full rerun.

The exact selected library passes all30 C API files,155cases,772,976assertions.
Each driver's runtime loader trace identifies the preserved library; its SHA remains
unchanged after the run (`capi-upstream-3/summary.json`). The existing exclusions remain.
The shell passes392tests/three identical skips and all395 test IDs/verdicts match the
C++ control (`shell-release-comparison-3.json`). Full differential: zero unexpected/
768cases, including46PTY, with the same three known gaps (`shell-diff-2.log`).

The obsolete `input-latin1-string` known marker now masks a matching case. Remove
that marker and rerun the unchanged case strictly. Keep the three remaining gaps
open and preserve separately labeled raw output for diagnosis. Workspace linting and
one fresh complete4870file run are next; final client/storage/performance gates remain.

## Fresh complete rerun

Workspace/all-target Clippy with warnings denied passes (`workspace-clippy-3.*`).
The stale stdin-Latin1 marker is removed; its unchanged case passes strictly
(`shell-strict-latin1-3.log`). Exactly three known markers remain. Their raw byte
outputs are preserved losslessly in `shell-known-raw-3/results.json`; these gaps remain
open. The shipping library retains the same547DuckDBexports with no test bridge.

A fresh complete4,870file run is now live under `full-corpus-2/`, using immutable
signed-images-3 and `external-source-root-2`. All4,870frozen test-file hashes were checked
again before launch. Its manifest pins the image, source checkpoint, root, reference,
ratchet and normal engine-thread settings. Run one worker with300s/file and keep all
evidence separate. The first full run is retained unchanged, without verdict merging.
Current complete source evidence:844modified/untracked nonignored files in
`corrected-source-checkpoint-3.json` and its SHA-recorded archive. Current scoped
verification index: `release-verification-3.json`. Wait for the complete run before
claiming a fresh corpus acceptance; all broader goal gates remain open.

## Recursive-glob fixture correction

Full-corpus-2 was deliberately interrupted after2,991completed files (exit143), not
accepted or merged into another run. One new failure was fixture layout:
`read_csv_glob_crawl.test_slow` expected8files but saw0 because the external working
view linked `data/`. C++ CrawlDirectoryLevel explicitly skips symbolic links. The
unmodified frozen C++ binary reproduces the same failure in that view
(`glob-fixture-root2-cpp/`). No engine glob semantics or test assertions were changed.

`external-source-root-3` now contains actual copied `test/` and `data/` directories.
All4,870test hashes and1,522data entries match the original source; data bytes total
121,870,559. The glob case and seven prior regressions all pass on both engines in
this view, eight/eight with no skips (`root3-controls-rust/`, `root3-controls-cpp/`).
All other frozen inputs are retained through links, with a separate actual test
scratch directory. The layout change preserves the recursive-glob fixture semantics.

A new complete run is live under `full-corpus-3/` with the same immutable signed Slt3,
one worker, normal engine threading and300s/file. `full-corpus-3/manifest.json` pins
all identities. `fixture-correction-3.json` and `full-corpus-2/interrupted-summary.json`
preserve why the second attempt was stopped. Only a terminal complete run plus exact
coverage/ratchet checks can establish fresh corpus acceptance.

## Terminal fresh corpus acceptance

Full-corpus-3 completes with4,781pass /15documented design failures /74sharedskips,
zero crashes/timeouts. All4,870file identities match the frozen C++ reference; all74
skip reasons match. The15failures are exactly the documented set left after removing
the six corrected functional failures from full-corpus-1. Ratchet:4,855listed,
zero regressions, zero newlypassing. Compare and ratchet both exit0. The signed image,
reference and ratchet hashes remain unchanged. All4,870test and1,522data hashes also
match their pre-run values. `full-corpus-3/terminal-summary.json` records these checks.
No old or interrupted verdict was merged. This closes the fresh corpus gate for
this snapshot; it does not close the broader port requirements.

The next black-box native argv probe is implemented and run serially against both
preserved binaries.32ASCII and32UTF8control cases match exactly. Across320cases,
170match /140mismatch /10fixture-error. All10fixture errors are host errno92 while
creating invalidUTF8filenames; these are not counted as parity passes or engine
failures. Malformed missing-path and database-path behavior remains measurable.
Raw output/exit codes and filename bytes are retained without normalization, and
worktrees are archived per engine. Evidence: external `shell-argv-20261025/checkpoint-1.json`.
Shared renderer changes require Python validation; the existing internal-error stack
trace gap remains unaccepted. Current evidence index: `release-verification-5.json`.

## Fresh complete current-core corpus (2026-10-25)

Signed runner4 (SHA8cece1a4d77516cf21e16d400a4ab9c924760f005a2cdd75ded3364a58fe36e7)
completed a fresh4870file run using one worker and normal engine threading. Results
are4781pass,15existing documented design failures and74skips whose reasons exactly
match frozen CPP. There are no crashes/timeouts, per-file statuses match the prior
snapshot, and compare/ratchet commands both exit0:4855listed/0regressions/0newpassing.
No earlier results are merged and no new exception is introduced.

Post-run verification confirms the selected runner/signature, frozen CPPreference,
ratchet list, all863recorded source files/archive,4870test files and1522data entries
(including two symlink targets) remain unchanged. Evidence:
`.context/artifacts/scalar-release-20261025/full-corpus-4/terminal-summary.json`, raw
results/worker logs/dashboard/ratchet and source-current-4.json/tar.gz. New Swift
preparation tooling/docs added during the run are outside the compiled engine snapshot.
This closes current scoped SQL corpus verification, not all-client/storage/performance
or whole-port completion. Remaining gates in the handoff still apply.


## Current post-Go-fix full refresh (2026-10-25, active)

After closing five Go-discovered core gaps, release all-target workspace Clippy passes,
signed SDK settings5/5and persistence10/10pass, current native C API30files/155cases/
749820assertions pass and the pinned Go root+Arrow suite passes652/652against the same
signed native image. All expected runtime images and unchanged hashes are recorded in
`.context/artifacts/go-client-20261025/verification-checkpoint-3.json`.

Native CLI/Slt rebuild exits0; immutable signed-images-5 records both images and the
879unchanged source inputs. CLI SHA61493f24b4106cae741bc0d3abafec74d39944b871d58e3d5f82db9621237f0f;
Slt SHAeb4a380f76c92eb374ec0cabcf05bf5c952114d1aa910dbd142bed88e691f526.
Current shell verification passes392upstream cases with3unchanged skips; complete
768-case differential/46PTY has0unexpected differences and2known OPEN gaps (internal
stack-trace output and catalog OID numbering). Do not call that complete shell parity.

Fresh full run5 starts2026-10-25T18:38:29UTC using the actual copied test/data root3,
one process worker, normal engine threading and new output/log/temp directories. All
4870test hashes and1522data entries including symlinks match, CPP reference/ratchet
hashes are unchanged. The first fixture audit's broad *.test* glob included the disabled
Spinque fixture; corrected auditing uses the actual harness's .test/.test_slow suffixes,
without changing the collector, test bodies or case set. Both audit records are retained.

Evidence root: `.context/artifacts/go-client-20261025/full-corpus-5`. The dedicated
run.py preserves manifest, logs, process exit, exact coverage, skip reasons, compare/
ratchet statuses and post-run source/fixture/binary/archive hashes. It never merges older
results. Worker live snapshots are partial observations only; acceptance requires the
terminal summary. Earlier corpus4/Python6/Swift verification remains historical evidence
on its recorded images. Final client/core/storage/performance gates remain open.
