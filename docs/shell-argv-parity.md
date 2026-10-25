# Native shell arguments — open parity work

The release3 snapshot still has a confirmed functional gap for invalid-UTF8 argv.
Raw bytes are preserved in
`.context/artifacts/scalar-release-20261025/shell-known-raw-3/results.json`.
For `-c` with a Latin1 string literal, C++ rejects invalid Unicode value construction;
Rust executes a replacement-character string. This gap remains unaccepted.

Source audit: `main.rs` turns `args_os` into `Vec<String>` with `to_string_lossy`.
CommandLineCall/OptionCallback retain String arguments. SQL execution already accepts
bytes, and a native dot-command dispatcher exists, but initial-command execution,
file/init paths and parts of callback state still take strings. State carriers include
program/database/init filenames, separators and NULL text; renderer settings and box
NULL configuration also hold String. Legacy dot-command callbacks still have a lossy
adapter. A transport-only patch to one SQL flag cannot establish full argv parity.

Before implementation, compare the frozen binary and preserved Rust3 across SQL
transports (-c/-s/-cmd/positional), valid ASCII/UTF8 controls, malformed UTF8, raw aliases,
unknown-option/program-name diagnostics, database/init/input paths, output separators
and NULL text across modes. Preserve raw stdout/stderr, exit codes and filename bytes.
POSIX argv cannot contain embedded NUL; do not label an untransportable case as an
engine mismatch. File fixtures must actually use the argument's native filename bytes.

Preserve raw bytes through each applicable boundary and let the corresponding C++
operation determine validation/error priority. Do not fix the string-literal case by
blanket UTF8 rejection of every argument. Changes to shared rendering carriers require
review of their SDK/Python consumers as well. This document is a source audit and test
scope, not runtime evidence for those still-unmeasured boundaries or a completion claim.
Run builds/tests serially after the active complete corpus terminates.

## Measured release3 baseline

The new `scripts/shell-argv-parity.py` executes actual byte argv and creates fixtures
with native filename bytes, preserving raw stdout/stderr, return codes, filename
bytes and per-engine worktrees. No version/address/decoder-byte normalization is used.
Database content is retained but excluded from this shell comparison, so this probe
is not a storage/WAL compatibility test. Crashes/timeouts/fixture errors never count
as parity passes. It requires new --work/--out paths under the external artifact task.

Both32-caseASCII and32-caseUTF8controls match. The full320-case default run records
170matches,140mismatches and10hostfixture errors. Five malformed-byte classes each
mismatch across all four SQL transports, raw print, eight NULL modes, separators,
newline, version errors, option/program-name errors, DB paths and missing-file errors.
JSON/quote NULL rendering ignores the configured text like C++, so those cases match.
The host rejects creation of invalidUTF8files with errno92; ten existing-file fixtures
are unavailable and explicitly held, while missing-path/DB-path behavior is measured.

Malformed boxed NULL text causes C++'s UTF8 width calculation to report an INTERNAL
error (exit1), including its Stack Trace section. Rust currently renders replacement
text (exit0). Preserve actual validation/error priority; the trace-format discrepancy
remains the previously open architectural diagnostic gap, without a new waiver.
All artifacts are under `.context/artifacts/shell-argv-20261025/`; `checkpoint-1.json`
contains source/binary hashes and exact family counts. The script can additionally
exercise all255nonNULbyte suffixes with --exhaustive; that matrix has not run yet.

## First native-byte implementation

OS argv, option callbacks, program/database/init filenames, NULL text and separators
now retain ByteString through their consumers. Initial SQL/dot dispatch takes bytes;
file opens use native paths; version lookup and option diagnostics preserve raw input.
Renderer row data/settings and escapes write bytes; aligned NULL width uses the
fallible native C++-compatible width path. Metadata .nullvalue/.separator now use native
callbacks. Other legacy dot callbacks remain open. Python's shared box configuration
assignment is adapted; its runtime validation remains required.

Workspace/all-target checking passes. The signed first release shell passes392upstream
tests/three existing skips. The unchanged320-case probe improves170to294exactmatches;
124prior mismatches become exact matches, with zero previous matches regressed.
16rawmismatches remain: exit codes and error text before C++'s Stack Trace match,
but the trace format is still different. All10hostfixture errors remain, without a
waiver. Evidence: `shell-argv-20261025/comparison-1.json`, rawafter-1.jsonl and source/
image SHA manifests. This is not full shell/client completion or a fresh core gate
for the changed serde helper. Shared Python and broader checks remain pending.

The original argument-latin1 differential case now matches both stdout/stderr and
exit1 exactly. Remove its obsolete known marker and repeat full differential checks;
only the database-OID and internal-stack-trace markers remain. Full malformed-byte
coverage and rendering consumer validation still need to run.

## Exhaustive byte and regression checks

The full768-case differential now passes with zero unexpected differences, including
46PTYsessions. Only two existing known markers remain (internal-error stack traces
and database OIDs). The upstream suite still has392pass/three identical skips.

The exhaustive probe runs8,480cases (the original10classes plus255nonNULbyte suffixes):
7,622exactmatches /592rawmismatches /266hostfixture errors. Every mismatch has the
same exit code and exact error text before C++'s Stack Trace section; all592 remain
rawmismatches, without a waiver. `exhaustive-classification-1.json` records this
classification separately from unchanged raw evidence. Host fixture errors are not
counted as engine failures or parity passes.

Whole-workspace/all-target Clippy with warnings denied passes. Signed native unit
suites pass (shell library4, serializer6, shell binary15; shared tests can overlap).
A fresh release Python wheel is building for shared-renderer validation; this is
not Python completion. `implementation-checkpoint-3.json` is the current evidence
index. Source/signature and previous controls remain preserved on the SSD.

## Shared Python renderer validation found additional work

The first signed release wheel installs with one verified Rust engine image. Existing
render comparisons retain18/19matches (the known string-columns reference bug).
A new32caseNULL/configuration probe initially matches10/32: six tab/newline/ANSI cases
expose a width regression, and18invalid-option cases expose frozen error-format gaps.
These are retained in `null-render-{cpp,rust}-1.json`, not hidden by the broader shell
results. C++ shared BoxRenderer uses Utf8Proc glyph widths; shell column layout uses
terminal-aware RenderLength. Restore the shared box width to a byte-native glyph
calculation, retaining terminal semantics only in the existing column path. Add
explicit duckbox coverage to the argv probe so this distinction is tested directly.

Frozen show() binds Optional[py::int_] and Optional[py::str], then casts values in its
body. Measurements confirm: floats/index-only objects are binding TypeErrors; negative
or overflowing integers are body RuntimeErrors; unencodable strings are RuntimeErrors.
Preserve strict string acceptance (bytes are rejected by BOTH engines), exact frozen
invocation diagnostics and validation priority. `show-options-cpp-1.json` and
`null-surrogate-cpp-1.json` retain the reference behavior. These fixes and fresh package
reruns are required before accepting the shared renderer change.

## Python correction results

The second signed release wheel fixes all32focusedNULL-rendering outcomes and all36
show-width binding/body-cast outcomes exactly, including cache/fetch behavior and
error text. Existing render cases remain18/19, with only the previously documented
string-columns reference discrepancy. The first failed wheel and all raw output are
preserved; no differences were normalized away.

Fresh complete fast suites retain the exact earlier test-ID/verdict sets: Arrow19
has3,203passing records /the same3heldDecimal assertions /35skip-xfail records across
3,241JUnit records; Arrow21 has3,453passing /the same3held /36skip-xfail across3,492.
No collection errors or new failures. Held discrepancies remain unaccepted. Evidence:
`python-validation-2.json` and both raw full-fast directories under the argv artifact task.

All-target workspace Clippy with warnings denied now passes (`clippy-4.*`). Lint cleanup
after wheel2 changes only a documentation code span and passing the bound receiver
by reference; a final release wheel rebuild is in progress so exact-image confirmation
can use current source. Updated CLI duckbox/exhaustive checks, package typing/API and
broader current-source/core/storage/client/performance gates remain required.

## Exact current Python image verification

Release wheel3 is signed and installed into new Arrow19/21 environments. Both load
exactly one Rust engine image; installed extension SHA matches the signed wheel
(7324ed059ebc80bbd96f2cae535c5d6c355027de3f4144761a80d8011b2b79e5).
Focused32NULL and36width cases still match C++ exactly. Existing rendering remains
18/19 with the same unaccepted reference string-columns discrepancy.

Both complete fast matrices on this exact image reproduce the earlier node/verdict
sets:3,203pass/3held/35skip-xfail over3,241records forArrow19, and3,453/3/36 over3,492
forArrow21. Zero collection errors/new failures. All385frozen runtime exports remain
available;29typing-only declarations remain separate. All seven packaged stub files
are byte-identical to the preserved frozen copies. Two strict positive typing clients
pass; two negative clients each report exactly four expected errors.

Evidence: `python-validation-3.json` indexes identity, focused, JUnit, availability,
stub and typing receipts. This is not full Python/client completion or acceptance of
the held discrepancies. Updated CLI reconstruction is now in progress to validate
the shared glyph-width correction with explicit duckbox coverage. Broader core,
storage/WAL/version/platform and final coupled performance acceptance remain open.

## Updated shared-renderer CLI verification

The lint-clean shared-renderer source is rebuilt and signed as CLI snapshot2
(SHA32248057ecc1e642a3a1af7bc0dc273f19534279b99eb37cc3e37d7b1fdca7e4).
The expanded probe explicitly includes duckbox:330cases yield302exactmatches,
18trace-format mismatches and10hostfixture errors. The8,745caseexhaustive matrix yields
7,813exactmatches /666trace-format mismatches /266hostfixture errors. Each raw mismatch
has matching exit codes and exact pre-StackTrace error text; all666 raw verdicts
remain mismatches, not waived or normalized into passes.

The updated CLI passes392upstream shell tests with the same three skips. Its full
768case/46PTYdifferential is now running separately. `verification-checkpoint-4.json`
indexes the signed CLI identity, raw matrices, Python exact-image results and remaining
work. Broader core/current-native/WAL/client/performance gates are still open.
