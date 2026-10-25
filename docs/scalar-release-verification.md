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
