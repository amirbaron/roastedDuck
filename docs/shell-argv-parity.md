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

## Native dot-path boundary baseline

A focused dot-path probe composes the same exact-byte recorder without replacing
engine behavior. Eight ASCII and eight UTF8 controls match. The80casebaseline has
40matches /35mismatches /fivehostfilename fixture errors. All five malformed-byte
classes mismatch in .open, .open --sql, .output, .once, .log, missing .read and .cd.
Raw bytes, file effects and exit codes are preserved in `dot-baseline-1.jsonl`.

Migrate these callbacks and their state/OS boundaries to native bytes: database and
output paths, output-file state, directory changes, read paths, .open SQL/error text,
and output pipe arguments. Preserve safe-mode checks, option priority, fallback DB
behavior, file/pipe close sequencing and existing valid-string callers. Native raw
path handling must reach the OS; do not replace invalid names or count errno92 host
fixture failures as parity passes. `.open --sql` must preserve its SQL bytes before
value construction rejects invalid Unicode. Remaining legacy callbacks are not
silently declared complete by these focused cases.

## Native dot-path implementation results

The measured path callbacks now retain bytes through .open SQL/value/errors, native
file/directory calls, output-file state and pipe arguments. The original80caseprobe
improves40to75exactmatches, closing all35mismatches with no old match regressed. Five
host filename-fixture errors remain explicit. The2,120caseexhaustive run has1,987exact
matches, zero mismatches and133hostfixture errors; those are not parity passes.

Workspace/all-target checking and warnings-denied Clippy pass. Signed shell binary
unit tests pass15/15. The new signed CLI3 passes392upstream tests/three shared skips;
its complete768case/46PTYdifferential is running separately. The previous CLI2 full
suite completed with zero unexpected differences and two retained known gaps.

CLI3 SHA:fd071a7461d1c97c57bbd8c46fd4f07d4e57e6ee80cc072ae18942ad143905ad.
`dot-verification-1.json` indexes raw probes, source/binary hashes and remaining work.
The CLI-only dot files are outside the library's four shared rendering modules, so
this phase does not change the verified Python wheel3 rendering code. Other legacy
callbacks, database-OID semantics and stack-trace formatting remain open; these
results are not whole-shell or whole-client completion. No table cache was added.

## Catalog/import byte baseline

The latest CLI3 full differential completes with zero unexpected/768cases,46PTY,
and the same two known diagnostic/OID gaps. A catalog/import probe then passes eight
ASCII/eight UTF8 controls. Its80casebaseline has40matches/40mismatches, with no host
fixture errors: malformed arguments differ across .schema/debug/indent, .tables,
.indexes, .dump, missing .import paths and .import table names. Raw evidence is in
`catalog-baseline-1.jsonl`; these are new measured gaps, not inferred passes.

Retain native bytes through SQL literals/identifiers, qualified-pattern parsing,
prepared values and import's libc++-ordered generic parameters. Preserve validation
at value construction rather than replacing malformed names. C++ RunSchemaDumpQuery
iterates an unsuccessful result, which raises a failed-fetch error containing the
underlying query error; Rust currently returns silently. Match that operation-specific
failure propagation, keeping BEGIN/COMMIT output and flag restoration behavior tied
to the actual control flow. Do not turn these40mismatches into known exceptions.

## Catalog/import native-byte results

The new signed CLI4 closes all40measured catalog/import mismatches. The unchanged
80caseprobe now has80exactmatches, with zero earlier matches regressed. The2,120case
exhaustive run is2,120exactmatches, zero mismatches, zero fixture errors, zero waivers.
This includes raw SQL debug output, malformed-pattern validation, dump failed-fetch
error propagation and successful imports into raw table identifiers.

Workspace/all-target checking and warnings-denied Clippy pass. Signed native shell
binary units pass15/15. Upstream shell tests pass392/three shared skips. Full768case /
46PTYdifferential for this CLI is now running; the preceding CLI3 run completed with
zero unexpected differences and the same two known diagnostic/OID gaps.

CLI4 SHA:7e015cb30dcd0286cec645bb9f57295fd97d0e79181ce6c2ab650e2110b3025c.
`catalog-verification-1.json` indexes source/image identities and all raw outcomes.
The changed CLI files remain outside the four shared rendering-library modules, so
this phase does not alter Python wheel3's verified rendering code. Additional legacy
callbacks, metadata-ID semantics, stack traces, remaining clients and final broad
storage/WAL/current-core/performance gates remain open. No table cache was added.

## Settings callback baseline

CLI4 completes the full differential with zero unexpected/768cases,46PTY and two
retained known diagnostic/OID gaps. A separate settings probe then passes20ASCII and
20UTF8controls. The200casebaseline has134matches/66mismatches, no fixture errors.
Raw boolean diagnostics, .help patterns, one-byte numeric separators and INSERT
names differ; measured numeric limit and fixed enum selectors already match.

Preserve byte-based boolean/integer parsing, separator byte counts, help GLOB matching
and native INSERT identifiers. Keep valid-string callers and all option validation
rules; do not blanket-reject malformed bytes. Numeric formatting can legitimately
reach the existing invalid-codepoint/InternalError path, whose raw stack trace remains
an open discrepancy. Retain that classification without waiving raw outcomes.

## Numeric-format carrier follow-up

The first settings implementation passes194/200cases (baseline134/200), closing60
mismatches. Six separator cases still differ: the native separator reaches state,
but FormatNumber converts its output through String::from_utf8_lossy. This causes
replacement text/success where C++ preserves the byte or raises an invalid-codepoint
error. Preserve rendered numeric cells and footer/readable-number strings as bytes;
compute widths through native glyph logic. The same helper is shared with table
metadata, so keep those consumers and Python renderer validation in scope. Do not
count a trace-prefix-only improvement as a raw parity pass.

## Settings and numeric-byte results

The second signed settings image (CLI6) records 196 exact matches and four raw
mismatches across the unchanged 200-case probe, compared with 134/66 before the
settings changes. All 62 newly exact cases were previous mismatches; no previous
exact match regressed. The exhaustive 5,300-case run records 5,296 exact matches,
the same four raw mismatches, and no fixture errors, crashes or timeouts.

Malformed decimal/thousand separators now follow C++ numeric rendering: incomplete
sequences retain their bytes, and invalid codepoints produce the matching exit/error
rather than replacement text. The four remaining mismatches have identical exit
codes, file effects and output before the Stack Trace section. They remain open raw
mismatches, with no waiver. Boolean diagnostics, help GLOB patterns, INSERT names,
separator byte counts and the existing numeric/enum selectors retain byte semantics.
Numeric cells, footer counts, readable numbers and table-size metadata retain bytes
through their render consumers.

All-target workspace checking and warnings-denied Clippy pass. Build6 terminates
successfully; the signed immutable CLI SHA is
79b8601a18d0b0fbb04aa28e523ef01594e92fe12ea61c622b3904cd2bda6b5a.
Evidence is under `.context/artifacts/shell-argv-20261025/`: `signed-image-6.json`,
`settings-implementation-2.json`, `settings-comparison-2.json`, both raw settings
runs and `settings-exhaustive-classification-1.json`. Shell regression checks and
fresh Python package validation are pending for this shared-renderer change.
The prior wheel3 results remain evidence for that earlier image. Remaining clients,
core/storage/WAL and final coupled speed-memory/per-query acceptance remain open;
no full-table cache has been added.

The numeric carrier follow-up also needs direct ordinary/signed-number, readable-cell,
large row-count and readable-footer journeys. Compare those with the same exact-byte
recorder on preserved CLI5 and corrected CLI6, including default malformed-byte classes.
Keep raw failures and prior images; a single ordinary decimal query cannot establish
that all changed numeric/footer consumers preserve their bytes.

## Numeric consumers and shell regression verification

Direct signed/readable-cell and row-count/readable-footer journeys improve from
62 to 72 exact matches across 80 cases, with zero previous matches regressed.
The eight remaining raw mismatches have the same exit/error prefix/file effects
and retain the open Stack Trace gap. Footer byte output matches C++, including
malformed separators where C++ emits those bytes. No additional validation policy
is imposed on those paths. Evidence: `numeric-comparison-1.json` and both raw runs.

The same signed CLI6 passes 392 upstream shell tests with three existing skips,
all 768 differential cases with zero unexpected differences (46 PTY sessions and
two known gaps), and signed unit suites of 15 binary/four shared tests. The default
argv, dot-path and catalog/import guard probes retain exactly their prior case IDs
and verdicts, including explicitly open raw mismatches and host fixture errors.
`settings-verification-1.json` indexes identities, raw results and limitations.
Fresh Python wheel4 validation is in progress for the changed shared numeric code;
this does not complete the remaining shell/core/client/storage/performance gates.
