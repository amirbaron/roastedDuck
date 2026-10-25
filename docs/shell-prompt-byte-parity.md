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
