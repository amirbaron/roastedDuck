# Linux x86_64 shell verification

The signed source-46 Rust CLI and frozen C++ 1.5.6 comparator each pass all 392
applicable upstream shell tests, with the same three skips. All 395 case IDs,
outcomes and skip reasons match. The original signed engines, upstream sources
and backward-compatibility database fixtures authenticate after execution.
This is Rosetta functional execution in the ARM Linux VM, not native x86 timing.

The first source-44 attempt had eight shared setup failures. Six opened old DBs
through a read-only reference mount; two require a working path containing
`duckdb`. Source 46 uses byte-identical writable copies of only the old-DB
fixtures and an appropriate working directory. No assertions or skips changed;
source 44's original failures remain preserved.

## Differential results and diagnosis

The original complete source-46 differential run has 39 differences in 768 cases,
including 46 PTY sessions. It is not a byte-parity pass. The categories are:

| Cases | Finding | Disposition |
|---|---|---|
| 18 | Binary path in usage/help/errors differs under translation | A separate same-path launch makes all 18 match without output normalization |
| 4 | Source ID/compiler identity | Preserve real build identities; explicitly documented differences |
| 1 | Native stack frames | Exact message/exit/stdout/files comparison passes; Linux execinfo format now fits the existing native-frame exception, with mutation guards |
| 1 | Display-color ordering | Same 256 colors/ANSI attributes in each of three modes; eight positions differ only among equal comparator keys; byte-order gap remains open |
| 12 | JSON errors | Ten match every JSON value and non-JSON byte apart from key order; two also differ in candidate-list order inside values; do not blanket-normalize JSON |
| 3 | Harness kills PTY children near five seconds | Separate longer observation confirms all six engine/case executions naturally return matching expected exit codes after about 5.2 seconds |

The alias diagnostic's two cases where both children were killed are not passes,
even though their raw records compare equal. The three original PTY failures remain preserved; corrected full-run 49 confirms
that the terminal-protocol repair removes all three shutdown mismatches.

Both engines send OSC 11 followed by DA1 while detecting terminal background
color. The harness answered only cursor-position queries, leaving DA1 unanswered
and entering DuckDB's five-second `select` timeout. Its original post-output
shutdown bound races that timeout. The harness now replies to DA1 as a terminal
without OSC 11 support and handles cursor/DA1 queries split across PTY reads.
It retains the exact emitted query bytes. Six harness tests now cover query fragmentation, repeated probes, unrelated
controls, relative work paths, missing startup markers and tightly scoped Linux
native-frame recognition. They pass. No timeouts,
assertions, case bodies, normalization or skips are weakened.

Each engine also launches through the same scratch symlink, preserving the
harness's intended equal program name even where binary translation substitutes
the executable path for argv[0]. This changes the actual launch path rather than
normalizing usage/error output. The complete fresh 768-case differential rerun finishes with 19 raw differences:
all 18 path mismatches and three shutdown kills disappear. One long-line case
exposes a separate startup race: keystrokes echo before the C++ startup banner
under translation. The long-line cases now require the actual banner before
sending the original keys, with a bounded readiness check; a missing marker fails
the case. This adds an explicit readiness assertion rather than changing outputs.
The relative `--work` launch regression from the new alias is also fixed.

A subsequent four-case targeted run passes with zero unexpected differences and
one existing native-frame exception. It covers both long-line widths, help output
through a relative work path, and the stack-frame case. The frame validator now
recognizes Linux execinfo records, still requiring identical message prefixes,
stdout, exit and files; unit mutations of those fields and malformed frames fail.
The original longer-observation PTY records also match every raw channel on both
engines. Full-run 49's 19 raw differences remain preserved, and targeted run 54
does not claim a fresh 768-case pass. Twelve JSON-order cases and one color-order
case remain open, alongside four real build-identity differences. Final complete
verification is still required after closing the remaining gaps.

Evidence under external `.context/artifacts/platform-matrix-20261029/`:

- `work/linux-amd64/shell-paired-44/`: original setup failures.
- `work/linux-amd64/shell-paired-46/verification.json`: full upstream and original differential collection.
- `work/linux-amd64/shell-alias-audit-47/`: all 39 original differing cases through a common executable alias, with raw bytes.
- `linux-amd64-shell-json-content-audit-47.json`: all object values and non-JSON channels compared.
- `linux-amd64-shell-color-order-audit-47.json`: color attributes and exact comparator-key audit.
- `work/linux-amd64/shell-pty-exit-48/`: longer-bound diagnosis and actual child states, without rewriting original tests.
- `work/linux-amd64/shell-diff-49/`: complete differential rerun with 19 raw differences.
- `linux-amd64-native-stack-{before,after}-53.json`: strict native-frame replay.
- `work/host-shell-relative-before-52/`: retained failing relative-path launch.
- `work/linux-amd64/shell-targeted-54/`: four targeted checks, zero unexpected differences and one scoped native-frame exception.
- `linux-amd64-shell-setting-candidates-audit-53.json`: both differing 159-name candidate lists have identical membership; string ordering remains open.

Final common-source shell/core/client/WAL/performance acceptance remains open.

## Source113 CLI behavioral component — 2026-10-31

The final source113 CLI-only candidate114 corrects the production catalog lookup
history: C++ constructs `MISSING_ENTRY` with its known position before name/type/
candidate insertion, so the logical position placeholder must retain that place.
The first synthetic ErrorData helper added it later; those distinct histories and
failed attempts remain retained. Strict common/function/shell/database checks and
all43 signed common tests pass.

Complete differential119 runs all768 cases including46PTY on the same signed CLI
and frozen C++ reference. It has **zero unexpected behavioral differences**.
Four raw cases still differ in declared source/compiler identities and the width
of the source-ID metadata table: cli-version, cli-version-double-dash, version,
and duckbox-pragma-version. The existing native-stack-frame exception is also
reported, with message/exit/stdout/files constrained to match. The raw exit1/four
difference count is retained; this is not described as zero raw byte differences.
All12JSON cases, palette ordering, candidate lists and interactive cases match.

The original full114 run had resize and wide-character PTY failures; targeted116
passes after startup/result readiness assertions. Full117 exposes another pre-banner
history-input race. All keyboard-driven manifest cases now require the existing
startup banner before input, and wide-character query output is required before
EOF. Missing markers fail the case. Parsed-case comparison verifies original command
bytes, args, fixtures, resize indices and output rules are preserved. Six harness
unit tests pass. The original failures are not overwritten or counted as passes.

Review receipt `linux-amd64-shell-behavior-review-119.json` records the exact four
metadata cases and log hash. `work/linux-amd64/shell-diff-119/verification.json`
retains all guarded inputs and native signature checks. A fresh full upstream395
case pair on this exactCLI also passes as shell-upstream120:392passes/three
matching skips perengine, with identical verdicts and skip reasons. Final common-source all-artifact SQL,
client, OS and coupled macro/outlier verification remains open.
