# Internal shell diagnostics

The current open-errors control matches CPPstdout, exit1 and the non-trace diagnostic
prefix/footer. CPPprints one native Stack Trace section; Rust omits it. Repeating CPP
against itself is byte-stable for this binary: the symbol names and instruction offsets
are build-specific, not evidence of runtime randomness. Raw controls are preserved in
`.context/artifacts/shell-final-gaps-20261025/open-error-controls-1.json` and stream files.

Frozen ErrorData::ConstructFinalMessage appends the standard internal-error explanation
and, when captured pointers exist, resolves the originating native trace. Rust already
matches the explanation. Add opt-in originating trace capture for the shell's missing
connection invariant and render a nonempty stack_trace extra-info field after that
explanation. Keep ordinary error construction/formatting, query values, state/exit behavior
and successful-query memory policy unchanged. Use actual Rust frames, not fabricated CPP
symbols or offsets; retain both raw streams and report any remaining frame difference.

Verify the actual null-connection journey, its stdout/exit/file effects and diagnostic
prefix/footer/trace presence. Trace rendering must preserve native error bytes and
cloned error data. Existing plain internal errors should remain unchanged without a
captured trace. Run appropriate signed common/shell tests and the full shell cohorts.
Do not remove the known difference merely by masking arbitrary trailing diagnostics.
The catalog OID difference and broader client/performance gates remain separate.
