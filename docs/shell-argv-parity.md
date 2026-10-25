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
