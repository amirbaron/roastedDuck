# Prompt, progress and UI command boundaries

The remaining prompt/progress callbacks convert native arguments to String. Parsed
prompt components and progress hide-patterns also store String, prompt SQL still uses
a lossy result adapter, and continuation/scroll prompts truncate at UTF8 boundaries
instead of C++'s19rawbytes. UI command state is String. These are source findings;
measure frozen C++1.5.6 and immutable CLI7 before implementation.

Use actual byte arguments and preserve raw outputs, exits and file effects. Cover
prompt/progress bracket names, settings, color/highlight errors, unterminated syntax,
max-length/min-size and progress alignment/hide options, plus a deliberately missing
custom UI procedure. Numeric controls must include signs, suffixes, overflow and
non-ASCII whitespace, since C++ uses std::stoull rather than Unicode trimming.

Add real terminal prompt journeys for literal/SQL prompts and continuation/scroll
bytes, including the19byte boundary. Suppress banners with .startup_text none and
use the existing explicit theme flag; require ASCII/UTF8 controls before parity claims.
Keep native validation/error priority, raw crashes and stack-trace mismatches visible.
Do not replace malformed bytes or reject all nonUTF8arguments. Configuration-only
progress probes do not prove progress display or nondeterministic timing equivalence.

Preserve signed images, source overlays and raw evidence on the SSD. One workload at
a time, nice10/Cargo-j1, no agents. Shared rendering-library code is outside the planned
CLI-only changes; expand Python verification if shared modules actually change.
The broader language-client, storage/WAL and final speed-memory gates remain open.

## Measured parsing/UI baseline

Signed CLI7 records90exact/102mismatches across192cases, with no fixture errors,
crashes or timeouts. In addition to malformed-byte diagnostics/custom UI names,
ASCII/UTF8numeric errors differ: C++ reports Invalid/stoull no-conversion or overflow,
while Rust reports Invalid Input/stoull. C++ accepts +1, rejects non-ASCII whitespace,
and maps -1 through unsigned conversion to the optional_idx invalid-sentinel error.
These are measured semantics, not waived controls. Evidence is in
`shell-argv-20261025/prompt-baseline-1.jsonl` and its manifest/summary.

Keep std::stoull's ASCII whitespace/sign/greedy decimal/overflow behavior and the
following optional_idx sentinel validation. Preserve bracket/value/literal bytes and
prompt SQL result bytes; continuation prompts use strncpy's19byte/C-string boundary.
Actual display and progress rendering still require separate validation.

## First implementation and terminal follow-up

The native parser/UI image (CLI8) improves90to190exact matches across192cases,
closing100mismatches with no previous exact match regressed. Exhaustive parsing/UI
is4,780/4,782exact, with two raw negative-sentinel Stack Trace differences still open;
exit/error-prefix/file effects match. Workspace/all-target check and Clippy pass.

The real26caseCLI7terminal baseline has14exact/12mismatches, including valid UTF8
crossing the19byte continuation boundary. CLI8's focused invalid-codepoint journeys
still expose the editor's forgiving width fallback: C++ exits1 while Rust proceeds.
A separate four-case CLI8 supplement finds a NUL returned by prompt SQL must terminate
the linenoise prompt C-string; the fallback reader correctly prints the full byte string.
Both max-width controls match. Keep those distinct boundaries: native editor widths
must retain C++ error behavior, and only linenoise's C-string input ends at NUL.
Evidence: `prompt-pty-baseline-1`, `prompt-pty-invalid-8`, `prompt-pty-supplement-8`
raw JSONL/worktrees under the existing SSD task. No raw differences are waived.
