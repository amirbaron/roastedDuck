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
